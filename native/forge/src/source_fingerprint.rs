//! Shared build/runtime source fingerprint for Forge's statically linked native crates.
//! Paths, lengths and bytes are framed, sorted and bounded. This is freshness
//! evidence for an operator-owned checkout, not an authenticated supply chain.
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

fn error(text: &str) -> io::Error {
    io::Error::other(text)
}

fn collect(root: &Path, relative: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let meta = fs::symlink_metadata(root.join(relative))?;
    if meta.is_dir() {
        for item in fs::read_dir(root.join(relative))? {
            collect(root, &relative.join(item?.file_name()), files)?;
        }
    } else if meta.is_file() {
        files.push(relative.to_path_buf());
    } else {
        return Err(error(
            "native source fingerprint refuses symlinks or special files",
        ));
    }
    if files.len() > 4096 {
        return Err(error("native source inventory exceeds 4096 files"));
    }
    Ok(())
}

pub fn files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for name in ["forge", "conductor-native", "slop-core"] {
        let base = PathBuf::from("native").join(name);
        for suffix in ["Cargo.toml", "Cargo.lock", "src"] {
            collect(root, &base.join(suffix), &mut files)?;
        }
        let build = base.join("build.rs");
        match fs::symlink_metadata(root.join(&build)) {
            Ok(_) => collect(root, &build, &mut files)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
    }
    for path in [
        // The CLI shares Forge's source directory and workspace lockfile, but
        // its manifest/stamp script are independent required build inputs.
        "native/forge-cli/Cargo.toml",
        "native/forge-cli/build.rs",
        "native/forge-cli/source_identity.rs",
        "ledger/routing_policy.toml",
        "native/forge/tests/fixtures/ledger/calibration.json",
    ] {
        collect(root, Path::new(path), &mut files)?;
    }
    files.sort();
    Ok(files)
}

/// Shared framing for a sorted source inventory, including Git-object readers.
pub struct Fingerprint {
    hash: Sha256,
    total: usize,
}
impl Default for Fingerprint {
    fn default() -> Self {
        Self::new()
    }
}
impl Fingerprint {
    pub fn new() -> Self {
        let mut hash = Sha256::new();
        hash.update(b"forge-native-source-v1\0");
        Self { hash, total: 0 }
    }
    pub fn add(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.total = self
            .total
            .checked_add(bytes.len())
            .ok_or_else(|| error("native source byte overflow"))?;
        if bytes.len() > 2 * 1024 * 1024 || self.total > 64 * 1024 * 1024 {
            return Err(error("native source byte budget exceeded"));
        }
        let name = path
            .to_str()
            .ok_or_else(|| error("native source path is not UTF-8"))?;
        self.hash.update((name.len() as u64).to_le_bytes());
        self.hash.update(name.as_bytes());
        self.hash.update((bytes.len() as u64).to_le_bytes());
        self.hash.update(bytes);
        Ok(())
    }
    pub fn finish(self) -> String {
        format!("{:x}", self.hash.finalize())
    }
}

pub fn fingerprint(root: &Path, files: &[PathBuf]) -> io::Result<String> {
    let mut fingerprint = Fingerprint::new();
    for path in files {
        let file = fs::File::open(root.join(path))?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() > 2 * 1024 * 1024 {
            return Err(error("native source file exceeds 2 MiB"));
        }
        let mut bytes = Vec::new();
        file.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        fingerprint.add(path, &bytes)?;
    }
    Ok(fingerprint.finish())
}
