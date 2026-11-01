//! Explicit, bounded source inventory and provenance validation for clerical drafts.
//! The core inventories sources; explicit submodules handle inference and output.
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub mod cli;
mod generate;
mod output;
mod validate;
pub use generate::{draft, GenerateOptions};
pub use output::{preflight_output, write_document};
pub use validate::{normalize_model_draft, validate_document, validate_value};

pub const MAX_SOURCE_COUNT: usize = 3;
pub const MAX_RETAINED_BYTES: usize = 64_000;
pub const MAX_TEXT_CHARS: usize = 2_400;
pub const MAX_DOCUMENT_BYTES: usize = 256_000;
pub const MODEL: &str = "qwen3.5:9b";
pub const APPLICATION: &str = "human review required; originals are preserved";
const MAX_PATH_BYTES: usize = 4_096;
const MAX_ARG_PATHS: usize = 1_024;

/// Deliberate finite admission, separate from the 64k retained-prefix bound.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub file_bytes: u64,
    pub aggregate_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            file_bytes: 64 * 1024 * 1024,
            aggregate_bytes: 128 * 1024 * 1024,
        }
    }
}

/// The caller resolves host/HOME/environment; this core never changes global state.
#[derive(Debug)]
pub struct Settings {
    host: PathBuf,
    roots: Vec<Root>,
    limits: Limits,
}
impl Settings {
    pub fn host(&self) -> &Path {
        &self.host
    }
    pub fn new(host: &Path, allowed_roots: &[PathBuf]) -> Result<Self> {
        Self::with_limits(host, allowed_roots, Limits::default())
    }
    pub fn with_limits(host: &Path, allowed_roots: &[PathBuf], limits: Limits) -> Result<Self> {
        ensure!(allowed_roots.len() <= 64, "too many clerk roots");
        ensure!(
            (1..=Limits::default().file_bytes).contains(&limits.file_bytes)
                && (1..=Limits::default().aggregate_bytes).contains(&limits.aggregate_bytes),
            "invalid clerk limits"
        );
        let host = host.canonicalize().context("resolve clerk host")?;
        ensure!(host.is_dir(), "clerk host must be a directory");
        let mut roots = vec![Root::pin(host.clone())?];
        for root in allowed_roots {
            // Absent optional home stores cannot contain a source. Do not create them.
            let root = match root.canonicalize() {
                Ok(root) => root,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("resolve clerk source root"),
            };
            ensure!(root.is_dir(), "clerk source root must be a directory");
            if !roots.iter().any(|held| held.path == root) {
                roots.push(Root::pin(root)?);
            }
        }
        Ok(Self {
            host,
            roots,
            limits,
        })
    }
    fn source_path(&self, raw: &Path) -> Result<PathBuf> {
        ensure!(
            raw.as_os_str().len() <= MAX_PATH_BYTES,
            "clerk path exceeds byte limit"
        );
        ensure!(
            raw.file_name().is_none_or(|v| v != ".current_work.md"),
            "direct .current_work.md ingestion is forbidden"
        );
        let path = if raw.is_absolute() {
            raw.to_owned()
        } else {
            self.host.join(raw)
        };
        let path = path
            .canonicalize()
            .context("resolve explicit clerk source")?;
        ensure!(path.to_str().is_some(), "clerk path must be UTF-8");
        ensure!(
            path.as_os_str().len() <= MAX_PATH_BYTES,
            "clerk path exceeds byte limit"
        );
        ensure!(
            path.file_name().is_none_or(|v| v != ".current_work.md"),
            "direct .current_work.md ingestion is forbidden"
        );
        ensure!(
            self.roots.iter().any(|root| path.starts_with(&root.path)),
            "source is outside the clerk catalog"
        );
        Ok(path)
    }
}

/// Capability pinning prevents ambient root replacement from redirecting reads.
/// An unconfined same-UID writer can still mutate any returned path afterwards;
/// callers must revalidate provenance before use. Ancestors of configured roots
/// and the process mount namespace remain operator-controlled trust boundaries.
#[derive(Debug)]
struct Root {
    path: PathBuf,
    directory: Dir,
    identity: Metadata,
}
impl Root {
    fn pin(path: PathBuf) -> Result<Self> {
        let directory = open_absolute_directory(&path)?;
        let identity = directory.dir_metadata()?;
        Ok(Self {
            path,
            directory,
            identity,
        })
    }
    fn check(&self) -> Result<()> {
        let current = open_absolute_directory(&self.path)?.dir_metadata()?;
        ensure!(
            same_inode(&self.identity, &current),
            "clerk source root changed"
        );
        Ok(())
    }
    fn open(&self, relative: &Path) -> Result<cap_std::fs::File> {
        let name = relative.file_name().context("source must name a file")?;
        let mut directory = self.directory.try_clone()?;
        for component in relative.parent().unwrap_or(Path::new("")).components() {
            let Component::Normal(name) = component else {
                anyhow::bail!("unsafe clerk source path");
            };
            directory =
                Dir::from_std_file(directory.open_with(name, &directory_options())?.into_std());
        }
        Ok(directory.open_with(
            name,
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK),
        )?)
    }
}
fn directory_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    options
}
fn open_absolute_directory(path: &Path) -> Result<Dir> {
    ensure!(
        path.is_absolute(),
        "clerk directory capability must be absolute"
    );
    let mut directory = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory =
                    Dir::from_std_file(directory.open_with(name, &directory_options())?.into_std())
            }
            _ => anyhow::bail!("unsafe clerk root path"),
        }
    }
    Ok(directory)
}
fn same_inode(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Boundary {
    Canonicalized,
    Read,
}

#[derive(Clone, Debug)]
pub struct Source {
    pub path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
    pub title: String,
    pub text: String,
    pub truncated: bool,
}
#[derive(Debug, Serialize)]
pub struct SourceMetadata {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub title: String,
    pub truncated_for_prompt: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_text_chars: Option<usize>,
}
impl Source {
    pub fn metadata(&self, prompt_text_chars: Option<usize>) -> SourceMetadata {
        SourceMetadata {
            path: self.path.to_str().expect("admitted UTF-8 path").to_owned(),
            sha256: self.sha256.clone(),
            size_bytes: self.size_bytes,
            title: self.title.clone(),
            truncated_for_prompt: self.truncated
                || self.size_bytes > self.text.len() as u64
                || prompt_text_chars.is_some_and(|n| n < self.text.chars().count()),
            prompt_text_chars,
        }
    }
}

pub fn read_sources(settings: &Settings, paths: &[PathBuf]) -> Result<Vec<Source>> {
    read_sources_checked(settings, paths, |_, _| Ok(()))
}
fn read_sources_checked(
    settings: &Settings,
    paths: &[PathBuf],
    mut boundary: impl FnMut(Boundary, &Path) -> Result<()>,
) -> Result<Vec<Source>> {
    ensure!(
        !paths.is_empty(),
        "at least one explicit source path is required"
    );
    ensure!(
        paths.len() <= MAX_ARG_PATHS,
        "too many clerk source arguments"
    );
    let mut seen = BTreeSet::new();
    let mut sources = Vec::new();
    let mut total = 0_u64;
    for raw in paths {
        let path = settings.source_path(raw)?;
        if !seen.insert(path.clone()) {
            continue;
        }
        ensure!(
            seen.len() <= MAX_SOURCE_COUNT,
            "local clerk accepts at most 3 distinct sources"
        );
        boundary(Boundary::Canonicalized, &path)?;
        sources.push(read_source(settings, path, &mut total, &mut boundary)?);
    }
    Ok(sources)
}

fn read_source(
    settings: &Settings,
    path: PathBuf,
    total: &mut u64,
    boundary: &mut impl FnMut(Boundary, &Path) -> Result<()>,
) -> Result<Source> {
    let root = settings
        .roots
        .iter()
        .find(|root| path.starts_with(&root.path))
        .context("source is outside the clerk catalog")?;
    root.check()
        .context("clerk source root changed before read")?;
    let relative = path.strip_prefix(&root.path)?;
    let mut file = root
        .open(relative)
        .context("open clerk source within allowed root")?;
    let before = file.metadata()?;
    ensure!(before.is_file(), "clerk source must be a regular file");
    ensure!(
        before.len() <= settings.limits.file_bytes,
        "clerk source byte limit exceeded"
    );
    ensure!(
        before.len() <= settings.limits.aggregate_bytes.saturating_sub(*total),
        "clerk aggregate byte limit exceeded"
    );
    let mut digest = Sha256::new();
    let mut prefix = Vec::with_capacity(MAX_RETAINED_BYTES);
    let mut buffer = [0_u8; 64 * 1024];
    let mut read = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        read = read
            .checked_add(count as u64)
            .context("clerk byte count overflow")?;
        ensure!(
            read <= settings.limits.file_bytes,
            "clerk source byte limit exceeded"
        );
        ensure!(
            read <= settings.limits.aggregate_bytes.saturating_sub(*total),
            "clerk aggregate byte limit exceeded"
        );
        digest.update(&buffer[..count]);
        let keep = count.min(MAX_RETAINED_BYTES - prefix.len());
        prefix.extend_from_slice(&buffer[..keep]);
    }
    boundary(Boundary::Read, &path)?;
    root.check()
        .context("clerk source root changed during read")?;
    let current = root
        .open(relative)
        .context("clerk source path changed during read")?
        .metadata()?;
    ensure!(
        stable(&before, &current),
        "clerk source path changed during read"
    );
    ensure!(
        stable(&before, &file.metadata()?) && read == before.len(),
        "source changed while being read"
    );
    *total += read;
    let decoded = String::from_utf8_lossy(&prefix);
    let title = title(&decoded, &path);
    let text = decoded.chars().take(MAX_TEXT_CHARS).collect();
    Ok(Source {
        path,
        sha256: format!("{:x}", digest.finalize()),
        size_bytes: read,
        title,
        text,
        truncated: read > MAX_RETAINED_BYTES as u64,
    })
}

fn stable(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn title(text: &str, path: &Path) -> String {
    static HEADING: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^#{1,6}\s+(.+?)\s*#*\s*$").expect("valid heading pattern")
    });
    text.split(|ch| {
        matches!(
            ch,
            '\n' | '\r'
                | '\u{000b}'
                | '\u{000c}'
                | '\u{001c}'
                | '\u{001d}'
                | '\u{001e}'
                | '\u{0085}'
                | '\u{2028}'
                | '\u{2029}'
        )
    })
    .find_map(|line| {
        HEADING
            .captures(line.trim())
            .map(|capture| capture[1].trim().to_owned())
    })
    .unwrap_or_else(|| {
        path.file_name()
            .expect("source filename")
            .to_str()
            .expect("admitted UTF-8 path")
            .to_owned()
    })
}

pub fn exact_duplicate_groups(sources: &[Source]) -> Vec<Vec<String>> {
    let mut seen = BTreeSet::new();
    sources
        .iter()
        .filter_map(|source| {
            if !seen.insert(&source.sha256) {
                return None;
            }
            let group: Vec<_> = sources
                .iter()
                .filter(|other| other.sha256 == source.sha256)
                .map(|other| other.path.to_str().expect("admitted UTF-8 path").to_owned())
                .collect();
            (group.len() > 1).then_some(group)
        })
        .collect()
}

pub fn inventory(settings: &Settings, paths: &[PathBuf]) -> Result<Value> {
    let sources = read_sources(settings, paths)?;
    Ok(
        json!({"schema_version":1,"authority":"deterministic-clerk-inventory",
        "sources":sources.iter().map(|s| s.metadata(None)).collect::<Vec<_>>(),
        "exact_duplicate_groups":exact_duplicate_groups(&sources)}),
    )
}

#[cfg(test)]
mod tests;
