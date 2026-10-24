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

/// The RECORD rows of the files a distribution installs into site-packages. Its
/// own `*.dist-info` metadata, the console scripts under `../bin` and any bytecode
/// carry install paths, installer caches and compile flags, so two venvs holding
/// identical code would otherwise differ.
fn installed_files(record: &[u8]) -> String {
    let text = String::from_utf8_lossy(record);
    let mut rows: Vec<&str> = text
        .lines()
        .filter(|row| {
            let path = row
                .split(',')
                .next()
                .unwrap_or_default()
                .trim_start_matches('"');
            let top = path.split('/').next().unwrap_or_default();
            !path.is_empty()
                && !path.starts_with("../")
                && !top.ends_with(".dist-info")
                && !path.contains("__pycache__/")
                && !path.ends_with(".pyc")
        })
        .collect();
    rows.sort_unstable();
    sha1_hex(rows.join("\n").as_bytes())
}

/// One entry per installed distribution: the digest of the files it installs, which
/// changes whenever any of them does. Editable installs point at source the recorder
/// already tracks file by file, and their RECORD names the checkout path, so they are
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
                    Ok(bytes) if manifest == "RECORD" => installed_files(&bytes),
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
    fn distributions_are_keyed_by_installed_code_and_editables_by_name() {
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
        let site_dirs = [site.to_str().unwrap().to_owned()];
        let dists = distributions(&site_dirs).unwrap();
        assert_eq!(dists["local-0.1"], "editable");
        assert_eq!(
            dists["fixed-1.0"],
            installed_files(b"fixed/__init__.py,sha256=x,1\n")
        );
        // Another venv's install of the same code: its scripts carry another shebang,
        // its metadata another install record, and it compiled bytecode.
        std::fs::write(
            fixed.join("RECORD"),
            concat!(
                "../../../bin/fixed,sha256=other-shebang,9\n",
                "fixed-1.0.dist-info/direct_url.json,sha256=other-url,7\n",
                "\"fixed-1.0.dist-info/RECORD\",,\n",
                "fixed/__pycache__/__init__.cpython-312.pyc,sha256=z,3\n",
                "fixed/__init__.py,sha256=x,1\n",
            ),
        )
        .unwrap();
        assert_eq!(
            distributions(&site_dirs).unwrap()["fixed-1.0"],
            dists["fixed-1.0"]
        );
        std::fs::write(fixed.join("RECORD"), "fixed/__init__.py,sha256=changed,1\n").unwrap();
        assert_ne!(
            distributions(&site_dirs).unwrap()["fixed-1.0"],
            dists["fixed-1.0"]
        );
        std::fs::remove_dir_all(&site).unwrap();
    }
}
