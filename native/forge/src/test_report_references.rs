//! Bounded reference corpora for the existing dead-test classifier.
use anyhow::{ensure, Context, Result};
use conductor_native::{dead_tests::inputs::Capture, project_config, project_paths};
use std::{collections::BTreeSet, fs, os::unix::fs::MetadataExt, path::Path};

const MAX_ENTRIES: usize = 100_000;
const MAX_CORPUS: usize = 32 * 1024 * 1024;
const SUFFIXES: &[&str] = &["toml", "json", "yml", "yaml", "sh", "md", "txt"];

#[derive(Debug, PartialEq)]
struct Tree {
    root: String,
    suffixes: Vec<String>,
    identity: Option<(u64, u64)>,
    files: Vec<String>,
}

fn inventory(root: &Path, relative: &str, suffixes: &[String]) -> Result<Tree> {
    crate::candidate_source::safe_path(relative.as_bytes())?;
    let named = root.join(relative);
    let metadata = match fs::symlink_metadata(&named) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut files = Vec::new();
    if let Some(metadata) = &metadata {
        ensure!(
            metadata.is_dir(),
            "reference root must be a directory: {relative}"
        );
        ensure!(
            named.canonicalize()? == named,
            "reference root contains a symlink"
        );
        let walker = walkdir::WalkDir::new(&named)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.file_name() != "__pycache__");
        for (index, entry) in walker.enumerate() {
            ensure!(
                index < MAX_ENTRIES,
                "reference inventory exceeds 100000 entries"
            );
            let entry = entry?;
            ensure!(entry.depth() <= 64, "reference inventory depth exceeds 64");
            if entry.file_type().is_dir() {
                continue;
            }
            let path = entry.path();
            if !path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| suffixes.iter().any(|v| v == s))
            {
                continue;
            }
            ensure!(
                entry.file_type().is_file(),
                "reference input must be a regular non-symlink file"
            );
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("reference path must be UTF-8")?;
            crate::candidate_source::safe_path(relative.as_bytes())?;
            files.push(relative.to_owned());
        }
        files.sort();
        let after = fs::symlink_metadata(&named)?;
        ensure!(
            (metadata.dev(), metadata.ino()) == (after.dev(), after.ino()),
            "reference root changed"
        );
    }
    Ok(Tree {
        root: relative.to_owned(),
        suffixes: suffixes.to_vec(),
        identity: metadata.map(|m| (m.dev(), m.ino())),
        files,
    })
}

fn config_surface(path: &str, excluded: &str) -> bool {
    if !excluded.is_empty() && path.starts_with(excluded) {
        return false;
    }
    [
        "Makefile",
        "pyproject.toml",
        "package.json",
        ".pre-commit-config.yaml",
        ".agent_hooks",
        "README.md",
        "AGENTS.md",
        "CLAUDE.md",
    ]
    .contains(&path)
        || path.ends_with(".sh")
        || path.ends_with(".yml")
        || path.ends_with(".yaml")
        || ([".github/", ".claude/", "conductor/"]
            .iter()
            .any(|p| path.starts_with(p))
            && Path::new(path)
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| SUFFIXES.contains(&s)))
}

fn captured_config(capture: &mut Capture) -> Result<Option<project_config::ProjectConfig>> {
    for path in ["forge.toml", "pyproject.toml"] {
        if capture.is_file(path)? {
            let bytes = capture.read_bytes(path)?;
            let text =
                std::str::from_utf8(&bytes).context("project configuration must be UTF-8")?;
            return (if path == "forge.toml" {
                project_config::parse_native(Path::new(path), text)
            } else {
                project_config::parse_legacy(Path::new(path), text)
            })
            .map(Some)
            .map_err(anyhow::Error::msg);
        }
        match fs::symlink_metadata(capture.root().join(path)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => anyhow::bail!("project configuration must be a regular file: {path}"),
        }
    }
    Ok(None)
}

fn corpus(capture: &mut Capture, paths: impl IntoIterator<Item = String>) -> Result<String> {
    let mut output = String::new();
    for (index, path) in paths.into_iter().enumerate() {
        let bytes = capture.read_bytes(&path)?;
        let text = String::from_utf8_lossy(&bytes);
        ensure!(
            output.len().saturating_add(text.len()).saturating_add(1) <= MAX_CORPUS,
            "reference text exceeds 32 MiB"
        );
        if index > 0 {
            output.push('\n');
        }
        output.push_str(&text);
    }
    Ok(output)
}

pub(crate) struct References {
    pub config: String,
    pub notes: String,
    trees: Vec<Tree>,
}

impl References {
    pub(crate) fn capture(capture: &mut Capture, tracked: &[String]) -> Result<Self> {
        let config = captured_config(capture)?;
        let notes = project_paths::resolve_key_from_config(config.as_ref(), "notes_root")
            .map_err(anyhow::Error::msg)?
            .0;
        let registry = project_paths::resolve_key_from_config(config.as_ref(), "mutation_registry")
            .map_err(anyhow::Error::msg)?
            .0;
        let parent = Path::new(&registry)
            .parent()
            .context("mutation registry has no parent")?;
        let excluded = if parent.as_os_str().is_empty() {
            String::new()
        } else {
            format!("{}/", parent.display())
        };
        // Record both negative and directory observations through the shared safe
        // lookup, including ancestors when a configured reference root is absent.
        capture.is_file(".claude")?;
        capture.is_file(&notes)?;
        let hooks = inventory(
            capture.root(),
            ".claude",
            &SUFFIXES.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )?;
        let notes = inventory(capture.root(), &notes, &["md".into()])?;
        let mut config_paths: BTreeSet<_> = tracked
            .iter()
            .filter(|path| config_surface(path, &excluded))
            .cloned()
            .collect();
        config_paths.extend(
            hooks
                .files
                .iter()
                .filter(|path| !path.starts_with(&excluded) || excluded.is_empty())
                .cloned(),
        );
        let config = corpus(capture, config_paths)?;
        let text = corpus(capture, notes.files.iter().cloned())?;
        Ok(Self {
            config,
            notes: text,
            trees: vec![hooks, notes],
        })
    }
    pub(crate) fn verify(&self, root: &Path) -> Result<()> {
        for tree in &self.trees {
            ensure!(
                inventory(root, &tree.root, &tree.suffixes)? == *tree,
                "reference inventory changed: {}",
                tree.root
            );
        }
        Ok(())
    }
    pub(crate) fn includes(&self, path: &str) -> bool {
        self.trees.iter().any(|tree| {
            path.strip_prefix(&tree.root)
                .is_some_and(|rest| rest.starts_with('/'))
                && Path::new(path)
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|suffix| tree.suffixes.iter().any(|s| s == suffix))
        })
    }
}

#[cfg(test)]
#[path = "test_report_references_tests.rs"]
mod tests;
