//! Pinned candidate file and operator-directory inputs. Directory bindings do
//! not hash recursive contents; operator-owned parents and contents must remain
//! stable. Same-UID hostile replacement is outside this binding's guarantee.
use anyhow::{ensure, Context, Result};
use cap_std::fs::Dir;
use conductor_native::candidate_cargo_lint::read_optional;
use git2::{ObjectType, Oid};
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::{self, Metadata},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

const MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
}

fn identity(meta: &Metadata) -> Identity {
    Identity {
        dev: meta.dev(),
        ino: meta.ino(),
        mode: meta.mode(),
        uid: meta.uid(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    identity: Identity,
    links: u64,
    len: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}

fn stamp(meta: &Metadata) -> Stamp {
    Stamp {
        identity: identity(meta),
        links: meta.nlink(),
        len: meta.len(),
        mtime: meta.mtime(),
        mtime_ns: meta.mtime_nsec(),
        ctime: meta.ctime(),
        ctime_ns: meta.ctime_nsec(),
    }
}

fn canonical(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && path.as_os_str().as_encoded_bytes().len() <= 4096
            && !path
                .as_os_str()
                .as_encoded_bytes()
                .iter()
                .any(u8::is_ascii_control)
            && path
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
        "input path must be bounded, absolute and normalized"
    );
    ensure!(
        path.canonicalize()? == path,
        "input path contains a symlink or is not canonical"
    );
    Ok(())
}

fn exclude(path: &Path, excluded: Option<&Path>) -> Result<()> {
    if let Some(excluded) = excluded {
        canonical(excluded)?;
        ensure!(
            !path.starts_with(excluded),
            "operator input is inside an excluded snapshot or scratch directory"
        );
    }
    Ok(())
}

/// Pinned directory identity only; no recursive content immutability is implied.
pub struct DirectoryBinding {
    path: PathBuf,
    dir: Dir,
    identity: Identity,
}

impl DirectoryBinding {
    pub fn open(path: &Path, snapshot_exclusion: Option<&Path>) -> Result<Self> {
        canonical(path)?;
        exclude(path, snapshot_exclusion)?;
        let meta = fs::symlink_metadata(path)?;
        ensure!(
            meta.is_dir()
                && (meta.uid() == 0 || meta.uid() == unsafe { libc::geteuid() })
                && meta.mode() & 0o022 == 0,
            "operator directory must be owned and not group/other writable"
        );
        let dir = Dir::open_ambient_dir(path, cap_std::ambient_authority())?;
        let binding = Self {
            path: path.to_path_buf(),
            dir,
            identity: identity(&meta),
        };
        binding.verify()?;
        Ok(binding)
    }

    pub fn verify(&self) -> Result<()> {
        canonical(&self.path)?;
        let fresh = fs::symlink_metadata(&self.path)?;
        let held = self.dir.dir_metadata()?;
        use cap_std::fs::MetadataExt as _;
        ensure!(
            fresh.is_dir()
                && identity(&fresh) == self.identity
                && held.dev() == self.identity.dev
                && held.ino() == self.identity.ino,
            "operator directory identity changed"
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
enum Expected {
    Sha256([u8; 32]),
    Blob(Oid),
}

pub struct BoundFile {
    parent: DirectoryBinding,
    name: String,
    expected: Expected,
    maximum: usize,
    stamp: Stamp,
    external: bool,
}

fn expected_sha256(value: &str) -> Result<[u8; 32]> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "expected lowercase SHA-256 hex"
    );
    let mut out = [0; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

fn expected_blob(value: &str) -> Result<Oid> {
    ensure!(
        value.len() == 40
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "expected lowercase Git blob OID"
    );
    Ok(Oid::from_str(value)?)
}

fn matches(expected: &Expected, bytes: &[u8]) -> Result<bool> {
    Ok(match expected {
        Expected::Sha256(value) => <[u8; 32]>::from(Sha256::digest(bytes)) == *value,
        Expected::Blob(oid) => Oid::hash_object(ObjectType::Blob, bytes)? == *oid,
    })
}

fn metadata(
    parent: &DirectoryBinding,
    name: &str,
    external: bool,
    maximum: usize,
) -> Result<Stamp> {
    let m = fs::symlink_metadata(parent.path.join(name))?;
    ensure!(
        m.is_file() && m.nlink() == 1 && m.len() <= maximum as u64,
        "input must be bounded, singly linked and regular"
    );
    if external {
        ensure!(
            (m.uid() == 0 || m.uid() == unsafe { libc::geteuid() }) && m.mode() & 0o022 == 0,
            "external input is not safely owned"
        );
    }
    Ok(stamp(&m))
}

fn capture(
    parent: DirectoryBinding,
    name: String,
    expected: Expected,
    maximum: usize,
    external: bool,
) -> Result<(BoundFile, Vec<u8>)> {
    ensure!(
        (1..=MAX_BYTES).contains(&maximum),
        "input limit must be 1..=32 MiB"
    );
    parent.verify()?;
    let before = metadata(&parent, &name, external, maximum)?;
    let bytes = read_optional(&parent.dir, &name, maximum)
        .map_err(anyhow::Error::msg)?
        .context("input disappeared")?;
    parent.verify()?;
    ensure!(
        before == metadata(&parent, &name, external, maximum)?,
        "input changed during capture"
    );
    ensure!(matches(&expected, &bytes)?, "input digest mismatch");
    Ok((
        BoundFile {
            parent,
            name,
            expected,
            maximum,
            stamp: before,
            external,
        },
        bytes,
    ))
}

impl BoundFile {
    pub fn verify(&self) -> Result<()> {
        self.parent.verify()?;
        ensure!(
            metadata(&self.parent, &self.name, self.external, self.maximum)? == self.stamp,
            "input identity changed"
        );
        let bytes = read_optional(&self.parent.dir, &self.name, self.maximum)
            .map_err(anyhow::Error::msg)?
            .context("input disappeared")?;
        self.parent.verify()?;
        ensure!(
            metadata(&self.parent, &self.name, self.external, self.maximum)? == self.stamp
                && matches(&self.expected, &bytes)?,
            "input changed or digest mismatch"
        );
        Ok(())
    }
}

fn filename(path: &Path) -> Result<String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("input filename must be UTF-8")?;
    ensure!(
        !name.is_empty() && !name.bytes().any(|b| b.is_ascii_control()),
        "invalid input filename"
    );
    Ok(name.into())
}

fn external_with_tmp(
    path: &Path,
    hash: &str,
    maximum: usize,
    snapshot: &Path,
    tmp: Option<&Path>,
) -> Result<(BoundFile, Vec<u8>)> {
    canonical(path)?;
    exclude(path, Some(snapshot))?;
    exclude(path, tmp)?;
    let parent = DirectoryBinding::open(path.parent().context("input has no parent")?, None)?;
    capture(
        parent,
        filename(path)?,
        Expected::Sha256(expected_sha256(hash)?),
        maximum,
        true,
    )
}

pub fn external(
    path: &Path,
    expected_sha256: &str,
    maximum: usize,
    snapshot: &Path,
) -> Result<(BoundFile, Vec<u8>)> {
    let tmp = env::var_os("TMPDIR").map(PathBuf::from);
    external_with_tmp(path, expected_sha256, maximum, snapshot, tmp.as_deref())
}

pub fn snapshot(
    root: &Path,
    relative: &str,
    expected_blob_oid: &str,
    maximum: usize,
) -> Result<(BoundFile, Vec<u8>)> {
    crate::candidate_source::safe_path(relative.as_bytes())?;
    canonical(root)?;
    let path = root.join(relative);
    let parent =
        DirectoryBinding::open(path.parent().context("snapshot input has no parent")?, None)?;
    ensure!(
        parent.path.starts_with(root),
        "snapshot input escaped its root"
    );
    capture(
        parent,
        filename(&path)?,
        Expected::Blob(expected_blob(expected_blob_oid)?),
        maximum,
        false,
    )
}

#[cfg(test)]
#[path = "candidate_inputs_tests.rs"]
mod tests;
