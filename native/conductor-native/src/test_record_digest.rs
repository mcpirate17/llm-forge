//! Content identities: git blob oids, `stat` keys, directory listings and the
//! installed-distribution part of the environment key.

use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub(crate) const ABSENT: &str = "absent";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn sha1_hex(bytes: &[u8]) -> String {
    hex(&Sha1::digest(bytes))
}

/// The oid `git hash-object` gives these bytes, so a recorded file matches a tree
/// entry without reading the blob.
pub(crate) fn blob_oid(content: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", content.len()).as_bytes());
    hasher.update(content);
    hex(&hasher.finalize())
}

/// Identity of a host file outside the repository. A rewrite changes the mtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Stat {
    size: u64,
    mtime_ns: i128,
    inode: u64,
}

impl Stat {
    pub(crate) fn of(meta: &Metadata) -> Self {
        Self {
            size: meta.size(),
            mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
            inode: meta.ino(),
        }
    }

    pub(crate) fn read(path: &Path) -> Option<Self> {
        std::fs::metadata(path).ok().map(|meta| Self::of(&meta))
    }

    pub(crate) fn key(&self) -> String {
        format!("stat:{}:{}:{}", self.size, self.mtime_ns, self.inode)
    }
}

/// Entry names a test could have observed, without bytecode caches.
pub(crate) fn listing(dir: &Path) -> std::io::Result<String> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name != "__pycache__" && !name.ends_with(".pyc") {
            names.push(name);
        }
    }
    names.sort();
    Ok(format!("list:{}", sha1_hex(names.join("\n").as_bytes())))
}

fn editable(dist_info: &Path) -> bool {
    let Ok(raw) = std::fs::read(dist_info.join("direct_url.json")) else {
        return false;
    };
    serde_json::from_slice::<serde_json::Value>(&raw)
        .ok()
        .and_then(|url| url.pointer("/dir_info/editable").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

/// One entry per installed distribution: its RECORD digest, which changes whenever
/// any installed file does. Editable installs point at source the recorder already
/// tracks file by file, and their RECORD names the checkout path, so they are
/// recorded by name only.
pub(crate) fn distributions(site_dirs: &[String]) -> std::io::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for site in site_dirs {
        let Ok(entries) = std::fs::read_dir(site) else {
            continue;
        };
        for entry in entries {
            let path = entry?.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let (stem, manifest) = if let Some(stem) = name.strip_suffix(".dist-info") {
                (stem.to_owned(), "RECORD")
            } else if let Some(stem) = name.strip_suffix(".egg-info") {
                (stem.to_owned(), "PKG-INFO")
            } else {
                continue;
            };
            let value = if editable(&path) {
                "editable".to_owned()
            } else {
                match std::fs::read(path.join(manifest)) {
                    Ok(bytes) => sha1_hex(&bytes),
                    Err(_) => ABSENT.to_owned(),
                }
            };
            out.insert(stem, value);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_oid_is_git_hash_object() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(
            blob_oid(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        assert_eq!(blob_oid(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn listings_ignore_bytecode_caches() {
        let dir = std::env::temp_dir().join(format!("test-record-listing-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("__pycache__")).unwrap();
        std::fs::write(dir.join("a.py"), "").unwrap();
        let with_cache = listing(&dir).unwrap();
        std::fs::remove_dir_all(dir.join("__pycache__")).unwrap();
        assert_eq!(listing(&dir).unwrap(), with_cache);
        std::fs::write(dir.join("b.py"), "").unwrap();
        assert_ne!(listing(&dir).unwrap(), with_cache);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn editable_distributions_are_recorded_by_name() {
        let site = std::env::temp_dir().join(format!("test-record-site-{}", std::process::id()));
        let fixed = site.join("fixed-1.0.dist-info");
        let edit = site.join("local-0.1.dist-info");
        std::fs::create_dir_all(&fixed).unwrap();
        std::fs::create_dir_all(&edit).unwrap();
        std::fs::write(fixed.join("RECORD"), "fixed/__init__.py,sha256=x,1\n").unwrap();
        std::fs::write(edit.join("RECORD"), "local.pth,sha256=y,1\n").unwrap();
        std::fs::write(
            edit.join("direct_url.json"),
            r#"{"url":"file:///x","dir_info":{"editable":true}}"#,
        )
        .unwrap();
        let dists = distributions(&[site.to_str().unwrap().to_owned()]).unwrap();
        assert_eq!(dists["local-0.1"], "editable");
        assert_eq!(
            dists["fixed-1.0"],
            sha1_hex(b"fixed/__init__.py,sha256=x,1\n")
        );
        std::fs::remove_dir_all(&site).unwrap();
    }
}
