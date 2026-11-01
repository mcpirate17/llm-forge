//! Bounded source access beneath a pinned graph host capability.
//! Binary source bytes are allowed. Same-UID writers can mutate names after
//! return; callers needing durable evidence must validate again before use.
//! Configured-host ancestors and the mount namespace are operator trust boundaries.
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, File, Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

const MAX_PATH_BYTES: usize = 4096;
const MAX_COMPONENTS: usize = 256;
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub struct TextSnapshot {
    pub text: String,
    pub sha256: String,
}

pub struct SourceRoot {
    host: PathBuf,
    directory: Dir,
    identity: Metadata,
    maximum: usize,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Boundary {
    Admitted,
    BeforeRead,
    Read,
}
impl SourceRoot {
    pub fn host(&self) -> &Path {
        &self.host
    }
    pub fn open(host: &Path) -> Result<Self> {
        Self::with_limit(host, MAX_SOURCE_BYTES)
    }
    fn with_limit(host: &Path, maximum: usize) -> Result<Self> {
        ensure!(
            (1..=MAX_SOURCE_BYTES).contains(&maximum),
            "invalid graph source byte limit"
        );
        ensure!(host.to_str().is_some(), "graph host path must be UTF-8");
        ensure!(
            host.as_os_str().len() <= MAX_PATH_BYTES,
            "graph host path exceeds byte limit"
        );
        let host = host.canonicalize().context("resolve graph source host")?;
        ensure!(
            host.to_str().is_some() && host.as_os_str().len() <= MAX_PATH_BYTES,
            "graph host path must be bounded UTF-8"
        );
        let directory = open_absolute_directory(&host)?;
        let identity = directory.dir_metadata()?;
        Ok(Self {
            host,
            directory,
            identity,
            maximum,
        })
    }
    /// Returns a host-relative path. UTF-8 is guaranteed by the raw string API;
    /// normalization never resolves a source symlink or accepts traversal.
    pub fn normalize(&self, raw: &str) -> Result<PathBuf> {
        ensure!(
            !raw.is_empty() && raw.len() <= MAX_PATH_BYTES && !raw.contains('\0'),
            "invalid graph source path or byte limit"
        );
        ensure!(
            !raw.split('/')
                .any(|part| matches!(part, "." | ".." | ".git")),
            "unsafe graph source path component"
        );
        let path = Path::new(raw);
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.host)
                .context("graph source is outside pinned host")?
        } else {
            path
        };
        let mut result = PathBuf::new();
        let mut count = 0;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                anyhow::bail!("unsafe graph source path");
            };
            ensure!(name != ".git", "unsafe graph source path component");
            count += 1;
            ensure!(
                count <= MAX_COMPONENTS,
                "graph source path component limit exceeded"
            );
            result.push(name);
        }
        ensure!(count > 0, "graph source must name a file");
        Ok(result)
    }
    pub fn hash(&self, raw: &str) -> Result<Option<String>> {
        self.hash_checked(raw, |_, _| Ok(()))
    }
    fn hash_checked(
        &self,
        raw: &str,
        boundary: impl FnMut(Boundary, &Path) -> Result<()>,
    ) -> Result<Option<String>> {
        Ok(self
            .read_checked(raw, self.maximum, false, boundary)?
            .map(|(digest, _)| digest))
    }
    /// Read text and its digest from the same bounded, identity-checked bytes.
    pub fn read_text(&self, raw: &str, maximum: usize) -> Result<Option<TextSnapshot>> {
        self.read_text_checked(raw, maximum, |_, _| Ok(()))
    }
    fn read_text_checked(
        &self,
        raw: &str,
        maximum: usize,
        boundary: impl FnMut(Boundary, &Path) -> Result<()>,
    ) -> Result<Option<TextSnapshot>> {
        ensure!(
            (1..=MAX_SOURCE_BYTES).contains(&maximum),
            "invalid graph source byte limit"
        );
        let Some((sha256, bytes)) =
            self.read_checked(raw, maximum.min(self.maximum), true, boundary)?
        else {
            return Ok(None);
        };
        let bytes = bytes.context("missing graph source text buffer")?;
        ensure!(!bytes.contains(&0), "graph source text contains NUL");
        let text = String::from_utf8(bytes).context("graph source text must be UTF-8")?;
        Ok(Some(TextSnapshot { text, sha256 }))
    }
    fn read_checked(
        &self,
        raw: &str,
        maximum: usize,
        collect: bool,
        mut boundary: impl FnMut(Boundary, &Path) -> Result<()>,
    ) -> Result<Option<(String, Option<Vec<u8>>)>> {
        let relative = self.normalize(raw)?;
        boundary(Boundary::Admitted, &relative)?;
        self.check_root()?;
        let Some(mut file) = self.open_file(&relative)? else {
            self.check_root()?;
            return Ok(None);
        };
        let before = file.metadata()?;
        ensure!(before.is_file(), "graph source must be a regular file");
        ensure!(
            before.len() <= maximum as u64,
            "graph source exceeds byte limit"
        );
        boundary(Boundary::BeforeRead, &relative)?;
        let (digest, count, bytes) = read_bytes(&mut file, maximum, collect)?;
        boundary(Boundary::Read, &relative)?;
        let current = self
            .open_file(&relative)?
            .context("graph source disappeared while hashing")?
            .metadata()?;
        ensure!(
            stable(&before, &current),
            "graph source path changed while hashing"
        );
        ensure!(
            stable(&before, &file.metadata()?) && count == before.len(),
            "graph source changed while hashing"
        );
        self.check_root()?;
        Ok(Some((digest, bytes)))
    }
    fn check_root(&self) -> Result<()> {
        let current = open_absolute_directory(&self.host)
            .context("graph source root changed")?
            .dir_metadata()?;
        ensure!(
            same_inode(&self.identity, &current),
            "graph source root changed"
        );
        Ok(())
    }
    fn open_file(&self, relative: &Path) -> Result<Option<File>> {
        let opened = (|| -> io::Result<File> {
            let mut directory = self.directory.try_clone()?;
            for component in relative.parent().unwrap_or(Path::new("")).components() {
                let Component::Normal(name) = component else {
                    unreachable!("normalized path");
                };
                directory =
                    Dir::from_std_file(directory.open_with(name, &directory_options())?.into_std());
            }
            let name = relative.file_name().expect("normalized file path");
            directory.open_with(
                name,
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK),
            )
        })();
        match opened {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("open graph source without symlinks"),
        }
    }
}
fn read_bytes(
    file: &mut File,
    maximum: usize,
    collect: bool,
) -> Result<(String, u64, Option<Vec<u8>>)> {
    let mut digest = Sha256::new();
    let mut bytes = collect.then(Vec::new);
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_usize;
    for _ in 0..=maximum {
        let length = buffer.len().min(maximum + 1 - total);
        let count = file.read(&mut buffer[..length])?;
        if count == 0 {
            return Ok((format!("{:x}", digest.finalize()), total as u64, bytes));
        }
        total += count;
        ensure!(
            total <= maximum,
            "graph source exceeds byte limit while hashing"
        );
        digest.update(&buffer[..count]);
        if let Some(bytes) = &mut bytes {
            bytes.extend_from_slice(&buffer[..count]);
        }
    }
    anyhow::bail!("graph source read iteration limit exceeded")
}
fn same_inode(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}
fn stable(a: &Metadata, b: &Metadata) -> bool {
    same_inode(a, b)
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn directory_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    options
}
fn open_absolute_directory(path: &Path) -> Result<Dir> {
    ensure!(path.is_absolute(), "graph source host must be absolute");
    let mut directory = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory =
                    Dir::from_std_file(directory.open_with(name, &directory_options())?.into_std())
            }
            _ => anyhow::bail!("unsafe graph source host path"),
        }
    }
    Ok(directory)
}

#[cfg(test)]
#[path = "graph_context_files_tests.rs"]
mod tests;
