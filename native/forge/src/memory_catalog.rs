//! Typed source catalogs and bounded, deterministic memory input inventories.
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, File, OpenOptions, OpenOptionsExt};
use conductor_native::{candidate_policy::glob_match, project_paths};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_FILES: usize = 100_000;
const MAX_EXAMINED: usize = 1_000_000;
const CHUNKER_VERSION: &str = "conductor-native/memory_chunking/v1";
const FALLBACK: &str = include_str!("../../../src/conductor/memory_sources.toml");

#[derive(Debug)]
pub struct Catalog {
    pub entries: Vec<Source>,
}

#[derive(Debug)]
pub struct Source {
    pub id: String,
    pub roots: Vec<PathBuf>,
    pub mode: String,
    pub recipe_sha256: String,
    globs: Vec<String>,
    exclude_globs: Vec<String>,
    exclude_dir_names: BTreeSet<String>,
    allow_file_root: bool,
}

#[derive(Debug)]
pub struct InputFile {
    pub source: String,
    pub path: PathBuf,
    pub root: PathBuf,
    pub relative: PathBuf,
    pub mode: String,
    pub recipe_sha256: String,
}

#[derive(Debug, Default)]
pub struct Inventory {
    pub files: Vec<InputFile>,
    pub missing_roots: Vec<(String, PathBuf)>,
    pub examined_entries: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    #[serde(rename = "source", alias = "sources")]
    sources: Vec<toml::Value>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    kind: String,
    root: Option<String>,
    roots: Option<Vec<String>>,
    absolute_root: Option<String>,
    include_dirs: Option<Vec<String>>,
    glob: Option<String>,
    #[serde(default)]
    globs: Vec<String>,
    #[serde(default)]
    exclude_globs: Vec<String>,
    #[serde(default)]
    exclude_dir_names: Vec<String>,
    #[serde(default = "heading")]
    chunk: String,
}
fn heading() -> String {
    "heading".into()
}

fn relative(raw: &str) -> Result<PathBuf> {
    ensure!(
        !raw.is_empty() && raw.len() <= 4096 && !raw.contains(['\\', '\0']),
        "invalid relative source path"
    );
    let path = Path::new(raw);
    ensure!(
        path.components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir)),
        "source path must be relative without traversal: {raw}"
    );
    Ok(path
        .components()
        .filter_map(|part| match part {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect())
}

fn absolute(raw: &str) -> Result<PathBuf> {
    ensure!(
        !raw.is_empty() && raw.len() <= 4096 && !raw.contains('\0'),
        "invalid absolute source path"
    );
    let expanded = if let Some(suffix) = raw.strip_prefix("~/") {
        let home = std::env::var_os("HOME").context("HOME required to expand source root")?;
        PathBuf::from(home).join(relative(suffix)?)
    } else if raw == "~" {
        PathBuf::from(std::env::var_os("HOME").context("HOME required to expand source root")?)
    } else {
        PathBuf::from(raw)
    };
    ensure!(
        expanded.is_absolute()
            && !expanded
                .components()
                .any(|part| matches!(part, Component::ParentDir)),
        "absolute_root must be absolute without traversal"
    );
    Ok(expanded.components().collect())
}

fn local_root(host: &Path, notes: &Path, raw: &str) -> Result<PathBuf> {
    let relative = relative(raw)?;
    if relative == Path::new("research/notes") {
        return Ok(notes.into());
    }
    Ok(host.join(relative))
}

fn roots(host: &Path, notes: &Path, entry: &Entry) -> Result<Vec<PathBuf>> {
    let shapes = usize::from(entry.root.is_some())
        + usize::from(entry.roots.is_some())
        + usize::from(entry.absolute_root.is_some());
    ensure!(shapes <= 1, "source {} has ambiguous roots", entry.id);
    ensure!(
        shapes == 1 || entry.include_dirs.is_some(),
        "source {} missing root",
        entry.id
    );
    let mut roots = if let Some(values) = &entry.roots {
        ensure!(
            entry.include_dirs.is_none(),
            "roots and include_dirs cannot be combined"
        );
        ensure!(
            !values.is_empty() && values.len() <= 256,
            "invalid root count"
        );
        values
            .iter()
            .map(|value| local_root(host, notes, value))
            .collect::<Result<Vec<_>>>()?
    } else {
        let base = if let Some(raw) = &entry.absolute_root {
            absolute(raw)?
        } else if let Some(raw) = &entry.root {
            local_root(host, notes, raw)?
        } else {
            host.into()
        };
        if let Some(values) = &entry.include_dirs {
            ensure!(
                !values.is_empty() && values.len() <= 256,
                "invalid include_dirs count"
            );
            values
                .iter()
                .map(|raw| Ok(base.join(relative(raw)?)))
                .collect::<Result<Vec<_>>>()?
        } else {
            vec![base]
        }
    };
    roots.sort();
    roots.dedup();
    Ok(roots)
}

fn patterns(values: &[String]) -> Result<()> {
    ensure!(
        values.len() <= 64
            && values.iter().all(|value| !value.is_empty()
                && value.len() <= 512
                && !value.contains(['/', '\\', '\0'])),
        "invalid filename glob list"
    );
    Ok(())
}

fn source(host: &Path, notes: &Path, entry: Entry) -> Result<Source> {
    ensure!(
        matches!(entry.chunk.as_str(), "heading" | "whole"),
        "unsupported chunk mode"
    );
    patterns(&entry.globs)?;
    patterns(&entry.exclude_globs)?;
    ensure!(
        entry.exclude_dir_names.len() <= 256,
        "too many excluded directory names"
    );
    for name in &entry.exclude_dir_names {
        ensure!(
            relative(name)?.components().count() == 1
                && Path::new(name).file_name().and_then(|value| value.to_str())
                    == Some(name.as_str())
                && !name.contains(['/', '\\'])
                && name != ".",
            "excluded directory must be a name"
        );
    }
    let roots = roots(host, notes, &entry)?;
    let allow_file_root = entry.absolute_root.is_some() && entry.include_dirs.is_none();
    let mut recipe =
        serde_json::json!({"source":entry,"roots":roots,"chunker_version":CHUNKER_VERSION});
    recipe.sort_all_objects();
    let recipe_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&recipe)?));
    let mut globs = entry.globs;
    if let Some(glob) = entry.glob {
        patterns(std::slice::from_ref(&glob))?;
        globs.push(glob);
    }
    if globs.is_empty() {
        globs.push("*.md".into());
    }
    ensure!(globs.len() <= 64, "too many filename globs");
    Ok(Source {
        id: entry.id,
        roots,
        mode: entry.chunk,
        recipe_sha256,
        globs,
        exclude_globs: entry.exclude_globs,
        exclude_dir_names: entry.exclude_dir_names.into_iter().collect(),
        allow_file_root,
    })
}

impl Catalog {
    pub fn load(host: &Path, explicit: Option<&Path>) -> Result<Self> {
        let host = host.canonicalize().context("resolve catalog host")?;
        ensure!(host.is_dir(), "catalog host must be a directory");
        let (path, configured) = if let Some(path) = explicit {
            (
                if path.is_absolute() {
                    absolute(path.to_str().context("catalog path must be UTF-8")?)?
                } else {
                    host.join(relative(
                        path.to_str().context("catalog path must be UTF-8")?,
                    )?)
                },
                true,
            )
        } else {
            project_paths::memory_sources(&host).map_err(anyhow::Error::msg)?
        };
        let text = match open_absolute(&path)
            .with_context(|| format!("memory-source catalog {}", path.display()))?
        {
            Some(Opened::File(file)) => {
                ensure!(
                    file.metadata()?.len() <= MAX_CATALOG_BYTES as u64,
                    "catalog exceeds 1 MiB"
                );
                let mut bytes = Vec::new();
                file.take(MAX_CATALOG_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                ensure!(bytes.len() <= MAX_CATALOG_BYTES, "catalog exceeds 1 MiB");
                String::from_utf8(bytes).context("catalog must be UTF-8")?
            }
            None if !configured => FALLBACK.into(),
            None => anyhow::bail!(
                "configured memory-source catalog {} does not exist",
                path.display()
            ),
            Some(Opened::Directory(_)) => {
                anyhow::bail!("memory-source catalog must be a regular file")
            }
        };
        let document: Document = toml::from_str(&text).context("invalid memory-source catalog")?;
        ensure!(
            document.schema_version == 1,
            "unsupported memory-source catalog schema"
        );
        ensure!(
            !document.sources.is_empty() && document.sources.len() <= 256,
            "catalog requires 1..256 sources"
        );
        let notes = project_paths::notes_root(&host).map_err(anyhow::Error::msg)?;
        let mut ids = BTreeSet::new();
        let mut entries = Vec::new();
        for raw in document.sources {
            let id = raw
                .get("id")
                .and_then(toml::Value::as_str)
                .context("source id required")?;
            crate::task_store::validate_id(id)?;
            ensure!(ids.insert(id.to_owned()), "duplicate source id {id}");
            let kind = raw
                .get("kind")
                .and_then(toml::Value::as_str)
                .context("source kind required")?;
            if kind == "index" {
                entries.push(source(
                    &host,
                    &notes,
                    raw.try_into().context("invalid index source")?,
                )?);
            }
        }
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(Self { entries })
    }

    pub fn scan(&self, selected: &BTreeSet<String>) -> Result<Inventory> {
        ensure!(
            selected
                .iter()
                .all(|id| self.entries.iter().any(|source| &source.id == id)),
            "unknown selected memory source"
        );
        let mut inventory = Inventory::default();
        let mut files = BTreeMap::new();
        for source in self
            .entries
            .iter()
            .filter(|source| selected.is_empty() || selected.contains(&source.id))
        {
            for root in &source.roots {
                scan_root(source, root, &mut inventory, &mut files)?;
            }
        }
        inventory.files = files.into_values().collect();
        inventory.missing_roots.sort();
        inventory.missing_roots.dedup();
        Ok(inventory)
    }
}

fn examined(inventory: &mut Inventory) -> Result<()> {
    inventory.examined_entries += 1;
    ensure!(
        inventory.examined_entries <= MAX_EXAMINED,
        "memory inventory exceeds 1M examined entries"
    );
    Ok(())
}

fn matches(source: &Source, name: &str) -> bool {
    source.globs.iter().any(|pattern| glob_match(name, pattern))
        && !source
            .exclude_globs
            .iter()
            .any(|pattern| glob_match(name, pattern))
}

fn candidate(
    source: &Source,
    path: &Path,
    root: &Path,
    files: &mut BTreeMap<(String, PathBuf), InputFile>,
) -> Result<()> {
    let path = path.to_owned();
    let key = (source.id.clone(), path.clone());
    if files.contains_key(&key) {
        return Ok(());
    }
    ensure!(
        files.len() < MAX_FILES,
        "memory inventory exceeds 100k files"
    );
    let relative = path.strip_prefix(root)?.to_owned();
    files.insert(
        key,
        InputFile {
            source: source.id.clone(),
            path,
            root: root.into(),
            relative,
            mode: source.mode.clone(),
            recipe_sha256: source.recipe_sha256.clone(),
        },
    );
    Ok(())
}

enum Opened {
    Directory(Dir),
    File(File),
}

fn options(directory: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(
        libc::O_NOFOLLOW | libc::O_NONBLOCK | if directory { libc::O_DIRECTORY } else { 0 },
    );
    options
}

// Open every component individually: a symlink in an ancestor cannot redirect
// catalog reads or scans, even when the final component itself is regular.
fn open_absolute(path: &Path) -> Result<Option<Opened>> {
    ensure!(
        path.is_absolute(),
        "catalog capability path must be absolute"
    );
    let mut directory = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    let mut parts = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .peekable();
    while let Some(name) = parts.next() {
        let file = match directory.open_with(name, &options(parts.peek().is_some())) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("open source component in {}", path.display()))
            }
        };
        let metadata = file.metadata()?;
        if parts.peek().is_none() && metadata.is_file() {
            return Ok(Some(Opened::File(file)));
        }
        ensure!(
            metadata.is_dir(),
            "source capability must be a regular file or directory"
        );
        directory = Dir::from_std_file(file.into_std());
    }
    Ok(Some(Opened::Directory(directory)))
}

fn descendant(root: &Dir, relative: &Path) -> Result<Dir> {
    let mut directory = root.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            anyhow::bail!("unsafe inventory directory");
        };
        directory = Dir::from_std_file(directory.open_with(name, &options(true))?.into_std());
    }
    Ok(directory)
}

fn scan_root(
    source: &Source,
    root: &Path,
    inventory: &mut Inventory,
    files: &mut BTreeMap<(String, PathBuf), InputFile>,
) -> Result<()> {
    examined(inventory)?;
    let directory = match open_absolute(root)? {
        None => {
            inventory
                .missing_roots
                .push((source.id.clone(), root.into()));
            return Ok(());
        }
        Some(Opened::File(_)) => {
            ensure!(
                source.allow_file_root,
                "only explicit absolute_root may name a file"
            );
            let name = root
                .file_name()
                .and_then(|value| value.to_str())
                .context("memory filename must be UTF-8")?;
            return if matches(source, name) {
                candidate(
                    source,
                    root,
                    root.parent().context("source file has no parent")?,
                    files,
                )
            } else {
                Ok(())
            };
        }
        Some(Opened::Directory(directory)) => directory,
    };
    let mut directories = vec![PathBuf::new()];
    while let Some(relative) = directories.pop() {
        let selected = descendant(&directory, &relative)?;
        let mut entries = Vec::new();
        for entry in selected.entries()? {
            examined(inventory)?;
            entries.push(entry?);
        }
        entries.sort_by_key(cap_std::fs::DirEntry::file_name);
        for entry in entries {
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let text = name.to_str().context("memory filename must be UTF-8")?;
            let path = relative.join(&name);
            ensure!(
                path.as_os_str().len() <= 4096,
                "memory path exceeds 4096 bytes"
            );
            if kind.is_dir() {
                if !source.exclude_dir_names.contains(text) {
                    directories.push(path);
                }
            } else {
                ensure!(
                    kind.is_file(),
                    "memory descendant must be a regular file or directory"
                );
                if matches(source, text) {
                    let opened = entry.open_with(&options(false))?;
                    ensure!(
                        opened.metadata()?.is_file(),
                        "memory file changed type while scanning"
                    );
                    candidate(source, &root.join(path), root, files)?;
                }
            }
        }
    }
    Ok(())
}
