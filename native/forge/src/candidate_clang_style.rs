//! Snapshot-bound clang-format styles. Inheritance, tags, aliases and ignore
//! files are refused. Operator-owned ancestor directories must remain stable.
use crate::candidate_source::{safe_path, Source, TreeEntry};
use anyhow::{bail, ensure, Context, Result};
use cap_std::fs::Dir;
use conductor_native::candidate_cargo_lint::read_optional;
use git2::{ObjectType, Oid};
use std::collections::{BTreeMap, BTreeSet};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use yaml_rust2::parser::{Event, Parser};

const MAX_FILES: usize = 100_000;
const MAX_PROBES: usize = 100_000;
const MAX_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONFIG: usize = 64 * 1024;
const MAX_CONFIG_TOTAL: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Style {
    Llvm,
    File(String),
}

/// The root Dir must remain pinned to the candidate snapshot for this binding's
/// lifetime. Call verify immediately before and after each subprocess batch.
pub struct Plan {
    root: Dir,
    root_path: PathBuf,
    probes: BTreeMap<String, Option<Oid>>,
    styles: BTreeMap<String, Style>,
}

fn admissible(bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes).context("style config must be UTF-8")?;
    let mut parser = Parser::new_from_str(text);
    let mut depth = 0usize;
    for _ in 0..20_000 {
        let (event, _) = parser
            .next_token()
            .map_err(|_| anyhow::anyhow!("invalid style YAML"))?;
        match event {
            Event::StreamEnd => return Ok(()),
            Event::Alias(_) => bail!("style YAML aliases are unsupported"),
            Event::Scalar(value, _, anchor, tag) => {
                ensure!(
                    anchor == 0 && tag.is_none(),
                    "style YAML anchors and tags are unsupported"
                );
                ensure!(
                    !value.to_ascii_lowercase().contains("inheritparentconfig"),
                    "style inheritance is unsupported"
                );
            }
            Event::SequenceStart(anchor, tag) | Event::MappingStart(anchor, tag) => {
                ensure!(
                    anchor == 0 && tag.is_none(),
                    "style YAML anchors and tags are unsupported"
                );
                depth += 1;
                ensure!(depth <= 64, "style YAML nesting exceeds 64");
            }
            Event::SequenceEnd | Event::MappingEnd => {
                depth = depth.checked_sub(1).context("invalid YAML nesting")?
            }
            _ => {}
        }
    }
    bail!("style YAML event limit exceeded")
}

fn config_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.into()
    } else {
        format!("{dir}/{name}")
    }
}

fn read_bound(root: &Dir, path: &str, expected: Option<Oid>) -> Result<Option<Vec<u8>>> {
    let bytes = read_optional(root, path, MAX_CONFIG).map_err(anyhow::Error::msg)?;
    match (&bytes, expected) {
        (None, None) => Ok(None),
        (Some(data), Some(oid)) if Oid::hash_object(ObjectType::Blob, data)? == oid => Ok(bytes),
        _ => bail!("style input differs from captured candidate tree: {path}"),
    }
}

fn inventory(entries: &[TreeEntry]) -> Result<BTreeMap<String, Oid>> {
    ensure!(
        entries.len() <= MAX_FILES,
        "candidate tree exceeds entry limit"
    );
    let mut result = BTreeMap::new();
    for entry in entries {
        let name = entry.path.rsplit('/').next().unwrap_or("");
        if !matches!(
            name,
            ".clang-format" | "_clang-format" | ".clang-format-ignore"
        ) {
            continue;
        }
        safe_path(entry.path.as_bytes())?;
        ensure!(
            entry.mode == "100644" || entry.mode == "100755",
            "style input must be a regular Git blob"
        );
        ensure!(
            entry.object_type == "blob",
            "style input must be a Git blob"
        );
        ensure!(
            result
                .insert(entry.path.clone(), Oid::from_str(&entry.oid)?)
                .is_none(),
            "duplicate style input"
        );
    }
    Ok(result)
}

fn parent_dirs(file: &str) -> Result<Vec<String>> {
    safe_path(file.as_bytes())?;
    let mut dirs = Vec::new();
    let mut current = file
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    loop {
        dirs.push(current.to_owned());
        if current.is_empty() {
            break;
        }
        current = current
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("");
    }
    Ok(dirs)
}

impl Plan {
    pub fn new(source: &Source, tree: &str, files: &[String]) -> Result<Self> {
        let entries = source.list_tree(tree)?;
        let root_path = source.root().to_path_buf();
        let root = Dir::open_ambient_dir(&root_path, cap_std::ambient_authority())?;
        Self::from_entries(root, root_path, &entries, files)
    }

    fn from_entries(
        root: Dir,
        root_path: PathBuf,
        entries: &[TreeEntry],
        files: &[String],
    ) -> Result<Self> {
        ensure!(
            !files.is_empty() && files.len() <= MAX_FILES,
            "selected file count outside bounds"
        );
        let tree = inventory(entries)?;
        let mut dirs = BTreeSet::new();
        let mut path_bytes = 0usize;
        let mut work = 0usize;
        for file in files {
            path_bytes = path_bytes
                .checked_add(file.len())
                .context("style path size overflow")?;
            ensure!(
                path_bytes <= MAX_PATH_BYTES,
                "style file paths exceed byte budget"
            );
            for dir in parent_dirs(file)? {
                work = work
                    .checked_add(1)
                    .context("style directory work overflow")?;
                ensure!(work <= 1_000_000, "style directory work exceeds limit");
                dirs.insert(dir);
                ensure!(
                    dirs.len() <= MAX_PROBES / 3,
                    "style directory inventory exceeds limit"
                );
            }
        }
        check_ambient_ignore(&root_path)?;
        let probes = bind_probes(&root, &tree, dirs)?;
        let mut styles = BTreeMap::new();
        for file in files {
            let style = parent_dirs(file)?
                .into_iter()
                .find_map(|dir| {
                    [".clang-format", "_clang-format"]
                        .into_iter()
                        .map(|name| config_path(&dir, name))
                        .find(|path| probes.get(path).is_some_and(Option::is_some))
                })
                .map(Style::File)
                .unwrap_or(Style::Llvm);
            ensure!(
                styles.insert(file.clone(), style).is_none(),
                "duplicate selected file"
            );
        }
        Ok(Self {
            root,
            root_path,
            probes,
            styles,
        })
    }

    pub fn argument_for(&self, path: &str) -> Result<String> {
        match self
            .styles
            .get(path)
            .context("file was not admitted by style binding")?
        {
            Style::Llvm => Ok("--style=LLVM".into()),
            Style::File(path) => Ok(format!(
                "--style=file:{}",
                self.root_path
                    .join(path)
                    .to_str()
                    .context("style path is not UTF-8")?
            )),
        }
    }

    pub fn verify(&self) -> Result<()> {
        check_ambient_ignore(&self.root_path)?;
        for (path, expected) in &self.probes {
            read_bound(&self.root, path, *expected)?;
        }
        Ok(())
    }
}

fn check_ambient_ignore(root: &Path) -> Result<()> {
    let mut parent = root.parent();
    while let Some(dir) = parent {
        match fs::symlink_metadata(dir.join(".clang-format-ignore")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
            Ok(_) => bail!("ambient clang-format ignore file is unsupported"),
        }
        parent = dir.parent();
    }
    Ok(())
}

fn bind_probes(
    root: &Dir,
    tree: &BTreeMap<String, Oid>,
    dirs: BTreeSet<String>,
) -> Result<BTreeMap<String, Option<Oid>>> {
    let mut probes = BTreeMap::new();
    let mut total = 0usize;
    for dir in dirs {
        for name in [".clang-format", "_clang-format", ".clang-format-ignore"] {
            let path = config_path(&dir, name);
            let expected = tree.get(&path).copied();
            if let Some(bytes) = read_bound(root, &path, expected)? {
                ensure!(
                    name != ".clang-format-ignore",
                    "clang-format ignore files are unsupported"
                );
                total = total
                    .checked_add(bytes.len())
                    .context("style config budget overflow")?;
                ensure!(
                    total <= MAX_CONFIG_TOTAL,
                    "style configs exceed aggregate byte budget"
                );
                admissible(&bytes)?;
            }
            probes.insert(path, expected);
        }
    }
    Ok(probes)
}

#[cfg(test)]
#[path = "candidate_clang_style_tests.rs"]
mod tests;
