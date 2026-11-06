//! Archive-derived extracted input inventory; maintained gzip/tar parsers only.
use super::{io, Package};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, MetadataExt, OpenOptions, OpenOptionsExt};
use flate2::bufread::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufReader, Read, Write},
    path::Path,
    time::Instant,
};
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Expanded {
    pub package: String,
    pub entries: Vec<io::Entry>,
}
struct Compressed {
    file: cap_std::fs::File,
    hash: Sha256,
    bytes: u64,
    maximum: u64,
    deadline: Instant,
}
impl Read for Compressed {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if Instant::now() > self.deadline {
            return Err(std::io::Error::other(
                "compressed seed I/O deadline exceeded",
            ));
        }
        let n = self.file.read(bytes)?;
        self.bytes += n as u64;
        if self.bytes > self.maximum {
            return Err(std::io::Error::other("archive grew while reading"));
        }
        self.hash.update(&bytes[..n]);
        Ok(n)
    }
}
struct ExpandedRead<'a, R> {
    inner: R,
    budget: &'a mut io::Budget,
    bytes: u64,
    maximum: u64,
}
impl<R: Read> Read for ExpandedRead<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.budget.check().map_err(std::io::Error::other)?;
        let max = bytes
            .len()
            .min(self.maximum.saturating_sub(self.bytes).saturating_add(1) as usize);
        let n = self.inner.read(&mut bytes[..max])?;
        self.bytes += n as u64;
        if self.bytes > self.maximum {
            return Err(std::io::Error::other("expanded archive exceeds 256MiB"));
        }
        self.budget
            .expand(n as u64)
            .map_err(std::io::Error::other)?;
        Ok(n)
    }
}
fn path(entry: &tar::Entry<'_, impl Read>, package: &str) -> Result<String> {
    let path = entry.path()?;
    let text = path.to_str().context("archive path is not UTF8")?;
    let text = text.strip_suffix('/').unwrap_or(text);
    io::relative(text)?;
    ensure!(
        text.split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".."),
        "archive path alias"
    );
    if text == package {
        return Ok(String::new());
    }
    let relative = text
        .strip_prefix(&format!("{package}/"))
        .context("archive wrong top directory")?;
    ensure!(
        relative != ".cargo-ok",
        "archive cannot supply Cargo completion sentinel"
    );
    Ok(relative.into())
}
fn insert(
    entries: &mut BTreeMap<String, io::Entry>,
    entry: io::Entry,
    budget: &mut io::Budget,
) -> Result<()> {
    for parent in Path::new(&entry.path)
        .ancestors()
        .skip(1)
        .filter(|p| !p.as_os_str().is_empty())
    {
        let path = parent
            .to_str()
            .context("nonUTF8 source parent")?
            .to_string();
        if let Some(old) = entries.get(&path) {
            ensure!(old.directory, "archive parent conflicts with file");
        } else {
            let value = io::Entry {
                path: path.clone(),
                directory: true,
                bytes: 0,
                sha256: None,
                executable: false,
            };
            budget.add(&value)?;
            entries.insert(path, value);
        }
    }
    if let Some(old) = entries.get(&entry.path) {
        ensure!(
            old.directory && entry.directory,
            "archive file/directory collision"
        );
    } else {
        budget.add(&entry)?;
        entries.insert(entry.path.clone(), entry);
    }
    Ok(())
}
fn entry_bytes<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    budget: &mut io::Budget,
    mut output: Option<cap_std::fs::File>,
) -> Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 65536];
    let mut total = 0u64;
    loop {
        budget.check()?;
        let n = entry.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        ensure!(total <= io::MAX_FILE, "source file bound exceeded");
        hash.update(&bytes[..n]);
        if let Some(output) = &mut output {
            output.write_all(&bytes[..n])?;
        }
    }
    ensure!(total == entry.size(), "archive entry size mismatch");
    if let Some(output) = output {
        output.sync_all()?;
    }
    Ok((total, format!("{:x}", hash.finalize())))
}
fn observe<R: Read>(
    archive: &mut tar::Archive<R>,
    package: &str,
    budget: &mut io::Budget,
    target: Option<&Dir>,
) -> Result<Vec<io::Entry>> {
    let mut entries = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for entry in archive.entries()? {
        budget.check()?;
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        ensure!(
            kind.is_file() || kind.is_dir(),
            "archive links/special/sparse entries unsupported"
        );
        if let Some(extensions) = entry.pax_extensions()? {
            for extension in extensions {
                let extension = extension?;
                let key = extension.key_bytes();
                ensure!(
                    !key.starts_with(b"GNU.sparse")
                        && key != b"SCHILY.realsize"
                        && key != b"SCHILY.filetype",
                    "sparse archive metadata unsupported"
                );
            }
        }
        let path = path(&entry, package)?;
        ensure!(seen.insert(path.clone()), "duplicate archive path");
        ensure!(
            seen.len() <= io::MAX_ENTRIES,
            "archive entry count exceeded"
        );
        ensure!(
            entry.size() <= io::MAX_FILE,
            "archive entry exceeds file bound"
        );
        if path.is_empty() {
            ensure!(kind.is_dir(), "archive top must be directory");
            continue;
        }
        ensure!(
            !kind.is_dir() || entry.size() == 0,
            "archive directory has data"
        );
        ensure!(
            entry.header().mode()? & 0o7000 == 0,
            "archive special permission bits unsupported"
        );
        let directory = kind.is_dir();
        let executable = !directory && entry.header().mode()? & 0o111 != 0;
        let output = if let Some(target) = target {
            if directory {
                io::mkdir(target, &path)?;
                None
            } else {
                Some(io::extracted_file(target, &path, executable)?)
            }
        } else {
            None
        };
        let (total, hash) = entry_bytes(&mut entry, budget, output)?;
        ensure!(!directory || total == 0, "archive directory has data");
        insert(
            &mut entries,
            io::Entry {
                path,
                directory,
                bytes: total,
                sha256: (!directory).then_some(hash),
                executable,
            },
            budget,
        )?;
    }
    ensure!(
        entries.get("Cargo.toml").is_some_and(|e| !e.directory),
        "archive package manifest missing"
    );
    Ok(entries.into_values().collect())
}
fn derive_one(
    root: &Dir,
    namespace: &str,
    package: &Package,
    budget: &mut io::Budget,
    inventory_budget: &mut io::Budget,
    maximum: u64,
    target: Option<&Dir>,
) -> Result<Expanded> {
    let parent = io::open_dir(root, &super::prefix(namespace, "cache"))?;
    let name = format!("{}-{}.crate", package.name, package.version);
    let before = parent.symlink_metadata(&name)?;
    ensure!(
        before.is_file() && before.nlink() == 1 && before.len() <= io::MAX_FILE,
        "unsafe locked archive"
    );
    let file = parent.open_with(
        &name,
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
    )?;
    ensure!(
        io::stamp(&before) == io::stamp(&file.metadata()?),
        "archive changed before decode"
    );
    let input = Compressed {
        file,
        hash: Sha256::new(),
        bytes: 0,
        maximum: before.len(),
        deadline: budget.deadline(),
    };
    let decoder = GzDecoder::new(BufReader::new(input));
    let mut archive = tar::Archive::new(ExpandedRead {
        inner: decoder,
        budget,
        bytes: 0,
        maximum,
    });
    let package_name = format!("{}-{}", package.name, package.version);
    // The reader budget is below both parsers, including GNU/PAX allocations.
    let entries = observe(&mut archive, &package_name, inventory_budget, target)?;
    let mut expanded = archive.into_inner();
    let mut bytes = [0u8; 65536];
    loop {
        let n = expanded.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        ensure!(
            bytes[..n].iter().all(|b| *b == 0),
            "nonzero data after tar terminator"
        );
    }
    let decoder = expanded.inner;
    let mut buffered = decoder.into_inner();
    ensure!(
        buffered.buffer().is_empty(),
        "trailing compressed archive data"
    );
    let mut extra = [0u8; 1];
    ensure!(
        buffered.read(&mut extra)? == 0,
        "trailing compressed stream"
    );
    let input = buffered.into_inner();
    ensure!(
        input.bytes == before.len()
            && format!("{:x}", input.hash.finalize()) == package.checksum
            && io::stamp(&before) == io::stamp(&input.file.metadata()?)
            && io::stamp(&before) == io::stamp(&parent.symlink_metadata(&name)?),
        "locked archive checksum/identity drift during decode"
    );
    Ok(Expanded {
        package: package_name,
        entries,
    })
}
pub(super) fn derive(root: &Dir, namespace: &str, packages: &[Package]) -> Result<Vec<Expanded>> {
    let mut budget = io::Budget::new();
    let mut result = Vec::new();
    let mut inventory_budget = io::Budget::new();
    let mut document = 0usize;
    let mut count = 0usize;
    for package in packages {
        let expanded = derive_one(
            root,
            namespace,
            package,
            &mut budget,
            &mut inventory_budget,
            io::MAX_FILE,
            None,
        )?;
        document += serde_json::to_vec(&expanded)?.len();
        count += expanded.entries.len();
        ensure!(
            document <= io::MAX_DOCUMENT / 2 && count <= io::MAX_ENTRIES,
            "expanded seed inventory exceeds manifest/count bound"
        );
        result.push(expanded);
    }
    Ok(result)
}
/// Materialize locked archives without tar metadata syscalls, then publish markers.
pub(super) fn materialize(
    source: &Dir,
    target: &Dir,
    namespace: &str,
    packages: &[Package],
    expected: &[Expanded],
) -> Result<()> {
    ensure!(
        packages.len() == expected.len(),
        "expanded package inventory count differs"
    );
    match target.symlink_metadata("registry/src") {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
        Ok(_) => anyhow::bail!("extracted source destination already exists"),
    }
    let mut budget = io::Budget::new();
    let mut inventory = io::Budget::new();
    let mut readback = io::Budget::new();
    for (package, expected) in packages.iter().zip(expected) {
        let path = format!(
            "registry/src/{namespace}/{}-{}",
            package.name, package.version
        );
        io::mkdir(target, &path)?;
        let directory = io::open_dir(target, &path)?;
        let observed = derive_one(
            source,
            namespace,
            package,
            &mut budget,
            &mut inventory,
            io::MAX_FILE,
            Some(&directory),
        )?;
        ensure!(
            &observed == expected,
            "staged archive differs from held expanded inventory"
        );
        let mut actual = io::generated_tree(target, &path, &mut readback)?;
        for entry in &mut actual {
            entry.path = entry
                .path
                .strip_prefix(&format!("{path}/"))
                .context("extracted readback path escaped package")?
                .to_owned();
        }
        ensure!(
            actual == expected.entries,
            "extracted source readback differs before completion marker"
        );
        let mut marker = io::extracted_file(&directory, ".cargo-ok", false)?;
        marker.write_all(b"{\"v\":1}")?;
        marker.sync_all()?;
    }
    verify(target, namespace, expected)
}
pub(super) fn verify(root: &Dir, namespace: &str, expected: &[Expanded]) -> Result<()> {
    let parent = "registry/src";
    match root.symlink_metadata(parent) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && expected.is_empty() => return Ok(()),
        Err(e) => return Err(e.into()),
        Ok(m) => ensure!(m.is_dir(), "registry/src must be a directory"),
    }
    let expected: BTreeMap<_, _> = expected
        .iter()
        .map(|p| (p.package.as_str(), &p.entries))
        .collect();
    let mut budget = io::Budget::new();
    let entries = io::generated_tree(root, parent, &mut budget)?;
    let mut actual = BTreeMap::<String, Vec<io::Entry>>::new();
    let mut namespaces = 0;
    for mut entry in entries {
        let full = entry.path.clone();
        let relative = full
            .strip_prefix("registry/src/")
            .context("bad generated source path")?;
        let mut parts = relative.splitn(3, '/');
        let group = parts.next().unwrap();
        ensure!(group == namespace, "unknown extracted registry namespace");
        let Some(package) = parts.next() else {
            ensure!(entry.directory, "registry namespace must be directory");
            namespaces += 1;
            continue;
        };
        ensure!(expected.contains_key(package), "unknown extracted package");
        let Some(path) = parts.next() else {
            ensure!(entry.directory, "extracted package must be directory");
            actual.entry(package.into()).or_default();
            continue;
        };
        entry.path = path.into();
        actual.entry(package.into()).or_default().push(entry);
    }
    ensure!(namespaces <= 1, "multiple extracted namespaces");
    ensure!(
        actual.keys().eq(expected.keys()),
        "extracted package set is incomplete"
    );
    for (package, mut entries) in actual {
        let marker = entries
            .iter()
            .position(|e| e.path == ".cargo-ok")
            .context("extracted package missing completion marker")?;
        let sentinel = entries.remove(marker);
        ensure!(
            !sentinel.directory
                && sentinel.bytes == 7
                && !sentinel.executable
                && sentinel.sha256.as_deref() == Some(super::sha(b"{\"v\":1}").as_str()),
            "invalid Cargo completion marker"
        );
        let expected = *expected.get(package.as_str()).unwrap();
        ensure!(
            &entries == expected,
            "extracted package contents/entry set changed: {package}"
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "mutation_cargo_seed_archive_tests.rs"]
mod tests;
