//! Shared build/runtime source fingerprint for Forge's statically linked native crates.
//! Paths, lengths and bytes are framed, sorted and bounded. This is freshness
//! evidence for an operator-owned checkout, not an authenticated supply chain.
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Input roots shared by live and immutable-tree inventory implementations.
pub const SOURCE_TREES: [&str; 3] = ["forge", "conductor-native", "slop-core"];
pub const EXTRA_FILES: [&str; 7] = [
    "native/forge-cli/Cargo.toml",
    "native/forge-cli/build.rs",
    "native/forge-cli/source_identity.rs",
    "ledger/routing_policy.toml",
    "native/forge/tests/fixtures/ledger/calibration.json",
    "src/conductor/memory_sources.toml",
    "src/conductor/embedding_routes.toml",
];

/// Select a regular file from the complete candidate tree inventory.
#[allow(dead_code)]
pub fn includes(path: &str) -> bool {
    EXTRA_FILES.contains(&path)
        || SOURCE_TREES.iter().any(|name| {
            let base = format!("native/{name}/");
            path.strip_prefix(&base).is_some_and(|suffix| {
                matches!(suffix, "Cargo.toml" | "Cargo.lock" | "build.rs")
                    || suffix.starts_with("src/")
            })
        })
}

fn error(text: &str) -> io::Error {
    io::Error::other(text)
}

const MAX_ENTRIES: usize = 16_384;
const MAX_DEPTH: usize = 128;

fn collect(
    root: &Path,
    relative: &Path,
    files: &mut Vec<PathBuf>,
    visited: &mut usize,
    depth: usize,
) -> io::Result<()> {
    *visited += 1;
    if *visited > MAX_ENTRIES || depth > MAX_DEPTH {
        return Err(error(
            "native source inventory exceeds entry or depth limit",
        ));
    }
    let meta = fs::symlink_metadata(root.join(relative))?;
    if meta.is_dir() {
        for item in fs::read_dir(root.join(relative))? {
            collect(
                root,
                &relative.join(item?.file_name()),
                files,
                visited,
                depth + 1,
            )?;
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
    let mut visited = 0;
    for name in SOURCE_TREES {
        let base = PathBuf::from("native").join(name);
        for suffix in ["Cargo.toml", "Cargo.lock", "src"] {
            collect(root, &base.join(suffix), &mut files, &mut visited, 0)?;
        }
        let build = base.join("build.rs");
        match fs::symlink_metadata(root.join(&build)) {
            Ok(_) => collect(root, &build, &mut files, &mut visited, 0)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
    }
    for path in EXTRA_FILES {
        collect(root, Path::new(path), &mut files, &mut visited, 0)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_counts_empty_directories_and_limits_depth() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("forge-inventory-{}-{nonce}", std::process::id()));
        fs::create_dir_all(root.join("one/two")).unwrap();
        let mut visited = MAX_ENTRIES - 1;
        let mut files = Vec::new();
        assert!(collect(&root, Path::new("one"), &mut files, &mut visited, 0).is_err());
        let mut visited = 0;
        assert!(collect(&root, Path::new("one"), &mut files, &mut visited, MAX_DEPTH).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
