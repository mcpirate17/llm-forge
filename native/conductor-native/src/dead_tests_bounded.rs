//! Interpreter-free, bounded import closure. Parse/read errors are explicit.
use super::{collect_python_imports, PythonModuleResolver};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path},
};

const MAX_FILES: usize = 100_000;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub module: String,
    pub importers: Vec<String>,
    pub tracked_importers: Vec<String>,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Scan {
    pub records: Vec<Finding>,
    pub files_scanned: usize,
    pub bytes_scanned: u64,
    pub sources: BTreeMap<String, String>,
    pub lookups: BTreeMap<String, bool>,
    pub root_identity: (u64, u64),
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && path.len() <= 4096
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
fn read_source(root: &Path, path: &str, total: &mut u64) -> Result<String> {
    ensure!(valid_path(path), "invalid import-closure path: {path}");
    let absolute = root.join(path);
    let canonical = absolute
        .canonicalize()
        .with_context(|| format!("resolve closure source {path}"))?;
    ensure!(
        canonical == absolute,
        "closure source contains a symlink: {path}"
    );
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&absolute)?;
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= MAX_FILE_BYTES,
        "closure source is not a bounded file: {path}"
    );
    *total = total
        .checked_add(before.len())
        .context("closure byte count overflow")?;
    ensure!(
        *total <= MAX_TOTAL_BYTES,
        "closure source byte budget exceeded"
    );
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES,
        "closure source grew beyond limit: {path}"
    );
    let after = file.metadata()?;
    let named = fs::symlink_metadata(&absolute)?;
    ensure!(
        named.is_file() && (named.dev(), named.ino()) == (after.dev(), after.ino()),
        "closure source name changed while reading: {path}"
    );
    ensure!(
        (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec()
        ) == (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec()
        ) && absolute.canonicalize()? == absolute,
        "closure source changed while reading: {path}"
    );
    String::from_utf8(bytes).with_context(|| format!("closure source is not UTF-8: {path}"))
}
fn safe_lookup(
    root: &Path,
    path: &str,
    seen: &mut BTreeMap<String, bool>,
    bytes: &mut usize,
) -> Result<bool> {
    ensure!(valid_path(path), "invalid import lookup path: {path}");
    let absolute = root.join(path);
    let mut parent = absolute.parent().context("import lookup parent missing")?;
    loop {
        match fs::symlink_metadata(parent) {
            Ok(meta) => {
                ensure!(
                    meta.is_dir() && !meta.file_type().is_symlink(),
                    "unsafe import parent"
                );
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                parent = parent.parent().context("import ancestor missing")?
            }
            Err(error) => return Err(error.into()),
        }
    }
    ensure!(
        parent.starts_with(root) && parent.canonicalize()? == parent,
        "import parent contains symlink"
    );
    let present = match fs::symlink_metadata(&absolute) {
        Ok(meta) => {
            ensure!(
                !meta.file_type().is_symlink(),
                "symlink import candidate refused: {path}"
            );
            meta.is_file()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if let Some(previous) = seen.get(path) {
        ensure!(
            *previous == present,
            "import lookup changed during scan: {path}"
        );
    } else {
        *bytes = bytes
            .checked_add(path.len())
            .context("import lookup bytes overflow")?;
        ensure!(
            seen.len() < 100_000 && *bytes <= 16 * 1024 * 1024,
            "import lookup budget exceeded"
        );
        seen.insert(path.into(), present);
    }
    Ok(present)
}
fn untracked(
    resolver: &PythonModuleResolver,
    importer: &str,
    dotted: &str,
    lookups: &mut BTreeMap<String, bool>,
    bytes: &mut usize,
) -> Result<Option<String>> {
    for path in PythonModuleResolver::candidates(dotted, importer) {
        if safe_lookup(&resolver.repo_root, &path, lookups, bytes)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}
fn record_safe(
    resolver: &PythonModuleResolver,
    importer: &str,
    base: &str,
    attribute: Option<&str>,
    output: &mut HashSet<String>,
    lookups: &mut BTreeMap<String, bool>,
    bytes: &mut usize,
) -> Result<()> {
    if let Some(attribute) = attribute.filter(|attribute| *attribute != "*") {
        let dotted = if base.is_empty() {
            attribute.to_owned()
        } else {
            format!("{base}.{attribute}")
        };
        if resolver.resolve(&dotted, importer).is_some() {
            return Ok(());
        }
        if let Some(path) = untracked(resolver, importer, &dotted, lookups, bytes)? {
            output.insert(path);
            return Ok(());
        }
    }
    if base.is_empty() || resolver.resolve(base, importer).is_some() {
        return Ok(());
    }
    if resolver.is_first_party(base) && !resolver.is_satisfied(base, importer) {
        if let Some(path) = untracked(resolver, importer, base, lookups, bytes)? {
            output.insert(path);
        }
    }
    Ok(())
}
/// Scan tracked Python imports and transitively discovered untracked modules.
/// No result is a gate verdict. Any invalid path, read, parse or budget failure is an error.
pub fn scan(repo_root: &Path, tracked: &[String]) -> Result<Scan> {
    ensure!(
        tracked.len() <= MAX_FILES && tracked.iter().all(|path| valid_path(path)),
        "invalid or excessive tracked paths"
    );
    let root = fs::canonicalize(repo_root).context("resolve closure repository")?;
    ensure!(root.is_dir(), "closure repository must be a directory");
    let root_stat = fs::metadata(&root)?;
    let identity = (root_stat.dev(), root_stat.ino());
    let resolver = PythonModuleResolver::new(&root, tracked);
    let tracked_set: HashSet<_> = tracked.iter().cloned().collect();
    ensure!(
        tracked_set.len() == tracked.len(),
        "duplicate tracked paths"
    );
    let mut importers = BTreeMap::<String, HashSet<String>>::new();
    let mut seen = HashSet::new();
    let mut frontier: Vec<_> = tracked
        .iter()
        .filter(|path| path.ends_with(".py"))
        .cloned()
        .collect();
    let mut total = 0;
    let mut sources = BTreeMap::new();
    let mut lookups = BTreeMap::new();
    let mut lookup_bytes = 0usize;
    while let Some(path) = frontier.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        ensure!(seen.len() <= MAX_FILES, "closure file budget exceeded");
        let source = read_source(&root, &path, &mut total)?;
        sources.insert(
            path.clone(),
            format!("{:x}", Sha256::digest(source.as_bytes())),
        );
        let statements = ruff_python_parser::parse_module(&source)
            .map_err(|error| anyhow::anyhow!("closure source does not parse: {path}: {error}"))?
            .into_syntax()
            .body;
        let mut imports = Vec::new();
        collect_python_imports(&statements, &path, &mut imports);
        let mut found = HashSet::new();
        for (base, attribute) in imports {
            record_safe(
                &resolver,
                &path,
                &base,
                attribute.as_deref(),
                &mut found,
                &mut lookups,
                &mut lookup_bytes,
            )?;
        }
        for dependency in found {
            importers
                .entry(dependency.clone())
                .or_default()
                .insert(path.clone());
            frontier.push(dependency);
            ensure!(
                frontier.len() <= MAX_FILES,
                "closure frontier budget exceeded"
            );
        }
    }
    let records = importers
        .into_iter()
        .map(|(module, paths)| {
            let mut importers: Vec<_> = paths.into_iter().collect();
            importers.sort_unstable();
            let tracked_importers = importers
                .iter()
                .filter(|path| tracked_set.contains(*path))
                .cloned()
                .collect();
            Finding {
                module,
                importers,
                tracked_importers,
            }
        })
        .collect();
    let after = fs::metadata(repo_root)?;
    ensure!(
        fs::canonicalize(repo_root)? == root && (after.dev(), after.ino()) == identity,
        "closure named root changed during scan"
    );
    Ok(Scan {
        records,
        files_scanned: seen.len(),
        bytes_scanned: total,
        sources,
        lookups,
        root_identity: identity,
    })
}
