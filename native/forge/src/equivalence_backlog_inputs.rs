//! Bounded, captured configuration and backlog inputs; repository data grants no writes.
use crate::candidate_inputs::{self, BoundFile, DirectoryBinding};
use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

pub(super) const FILE_LIMIT: usize = 16 * 1024 * 1024;
const CONFIG_LIMIT: usize = 1024 * 1024;

pub(super) struct Input {
    pub path: PathBuf,
    pub held: Option<BoundFile>,
}
impl Input {
    pub fn capture(path: PathBuf, limit: usize) -> Result<(Self, Option<Vec<u8>>)> {
        let captured = match std::fs::symlink_metadata(&path) {
            Ok(_) => Some(
                candidate_inputs::repository_live(&path, limit)
                    .with_context(|| format!("capture {}", path.display()))?,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).with_context(|| path.display().to_string()),
        };
        let (held, bytes) = match captured {
            Some((held, bytes)) => (Some(held), Some(bytes)),
            None => (None, None),
        };
        Ok((Self { path, held }, bytes))
    }
    pub fn verify(&self) -> Result<()> {
        match &self.held {
            Some(held) => held
                .verify()
                .with_context(|| self.path.display().to_string()),
            None => match std::fs::symlink_metadata(&self.path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error).with_context(|| self.path.display().to_string()),
                Ok(_) => bail!("previously absent input appeared: {}", self.path.display()),
            },
        }
    }
}

pub(super) struct Host {
    pub root: PathBuf,
    pub binding: DirectoryBinding,
    pub inputs: Vec<Input>,
    config: BTreeMap<String, String>,
}

fn probe(doc: &toml::Value) -> Result<BTreeMap<String, String>> {
    let mut value = doc;
    for key in ["tool", "forge", "equivalence_probe"] {
        let Some(next) = value
            .as_table()
            .context("host configuration must contain tables")?
            .get(key)
        else {
            return Ok(BTreeMap::new());
        };
        value = next;
    }
    let mut paths = BTreeMap::new();
    for (key, value) in value
        .as_table()
        .context("equivalence_probe must be a table")?
    {
        match key.as_str() {
            "waivers" | "ledger" | "report" | "run_artifacts" => {
                paths.insert(
                    key.clone(),
                    value
                        .as_str()
                        .with_context(|| format!("{key} must be a string"))?
                        .into(),
                );
            }
            "jobs" if value.is_integer() => (),
            "per_module_timeout" if value.is_integer() || value.is_float() => (),
            _ => bail!("[tool.forge.equivalence_probe] {key} is not a known key of its type"),
        }
    }
    Ok(paths)
}

pub(super) fn absolute(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)?;
    ensure!(
        path.as_os_str().len() <= 4096
            && !path
                .as_os_str()
                .as_encoded_bytes()
                .iter()
                .any(u8::is_ascii_control)
            && path
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
        "path must be bounded and normalized without traversal: {}",
        path.display()
    );
    Ok(path)
}

fn configured(root: &Path, value: &str) -> Result<PathBuf> {
    let expanded = if value == "~" || value.starts_with("~/") {
        let home =
            PathBuf::from(std::env::var_os("HOME").context("configured path requires HOME")?);
        home.join(value.strip_prefix("~/").unwrap_or(""))
    } else {
        ensure!(
            !value.starts_with('~'),
            "named-user tilde paths are unsupported; supply an absolute path"
        );
        root.join(value)
    };
    absolute(&expanded)
}

impl Host {
    pub fn capture(start: &Path) -> Result<Self> {
        let start = absolute(start)?;
        let start_binding = DirectoryBinding::repository(&start, None)?;
        let mut inputs = Vec::new();
        let mut root = start.clone();
        let mut first = None;
        let mut selected = None;
        for (index, dir) in start.ancestors().enumerate() {
            ensure!(
                index < 64,
                "host configuration ancestry exceeds 64 directories"
            );
            let (input, bytes) = Input::capture(dir.join("pyproject.toml"), CONFIG_LIMIT)?;
            inputs.push(input);
            let doc = bytes
                .map(|bytes| -> Result<toml::Value> {
                    Ok(toml::from_str(std::str::from_utf8(&bytes)?)?)
                })
                .transpose()?;
            if index == 0 {
                first = doc.clone();
            }
            if let Some(doc) = doc {
                if let Some(table) = conductor_native::test_record_config::parse_document(&doc)
                    .map_err(anyhow::Error::msg)?
                {
                    conductor_native::test_record_config::store_path(
                        dir,
                        &table.store,
                        std::env::var("HOME").ok().as_deref(),
                    )
                    .map_err(anyhow::Error::msg)?;
                    root = dir.to_path_buf();
                    selected = Some(doc);
                    break;
                }
            }
        }
        let config = selected
            .or(first)
            .as_ref()
            .map(probe)
            .transpose()?
            .unwrap_or_default();
        start_binding.verify()?;
        let binding = DirectoryBinding::repository(&root, None)?;
        Ok(Self {
            root,
            binding,
            inputs,
            config,
        })
    }
    pub fn path(&self, key: &str) -> Result<Option<PathBuf>> {
        self.config
            .get(key)
            .map(|value| configured(&self.root, value))
            .transpose()
    }
    pub fn output(&self, key: &str, supplied: Option<&Path>) -> Result<PathBuf> {
        let path = match supplied {
            Some(path) => absolute(path)?,
            None => {
                let path = self
                    .path(key)?
                    .with_context(|| format!("backlog: no {key}; pass --{key} or configure it"))?;
                ensure!(
                    path.starts_with(&self.root),
                    "configured {key} is outside the host; authorize it explicitly with --{key}"
                );
                path
            }
        };
        ensure!(path.file_name().is_some(), "{key} must name a file");
        Ok(path)
    }
    pub fn verify(&self) -> Result<()> {
        self.binding.verify()?;
        for input in &self.inputs {
            input.verify()?;
        }
        Ok(())
    }
}
