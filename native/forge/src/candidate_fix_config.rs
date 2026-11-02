//! Bind Ruff's hierarchical configuration inputs, including absent candidates.
use crate::{
    candidate_execution_workspace::Grants,
    candidate_inputs::{self, BoundFile},
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
const MAX_CONFIG: usize = 1024 * 1024;
const MAX_CONFIGS: usize = 1024;

pub(crate) struct Configs {
    files: BTreeMap<PathBuf, BoundFile>,
    absent: BTreeSet<PathBuf>,
    bytes: usize,
    digests: BTreeMap<PathBuf, String>,
    settings: BTreeSet<PathBuf>,
}
impl Configs {
    pub(crate) fn capture(
        root: &Path,
        paths: &[PathBuf],
        grants: &Grants,
        pins: &[String],
    ) -> Result<Self> {
        let pins = parse_pins(pins)?;
        let mut held = Self {
            files: BTreeMap::new(),
            absent: BTreeSet::new(),
            bytes: 0,
            digests: BTreeMap::new(),
            settings: BTreeSet::new(),
        };
        for path in paths {
            let absolute = root.join(path);
            for ancestor in absolute
                .parent()
                .context("source parent absent")?
                .ancestors()
            {
                for name in [".ruff.toml", "ruff.toml", "pyproject.toml"] {
                    let config = ancestor.join(name);
                    match fs::symlink_metadata(&config) {
                        Ok(_) => {
                            held.add(&config, root, grants, &pins, &mut BTreeSet::new())?;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            held.absent.insert(config);
                        }
                        Err(error) => return Err(error).context("inspect Ruff configuration"),
                    }
                    ensure!(
                        held.files.len() + held.absent.len() <= MAX_CONFIGS,
                        "Ruff configuration discovery exceeds 1024 candidates"
                    );
                }
            }
        }
        held.verify()?;
        Ok(held)
    }
    fn add(
        &mut self,
        path: &Path,
        root: &Path,
        grants: &Grants,
        pins: &BTreeMap<PathBuf, String>,
        visiting: &mut BTreeSet<PathBuf>,
    ) -> Result<bool> {
        ensure!(
            visiting.len() < 32 && visiting.insert(path.to_owned()),
            "cyclic/overlong Ruff extend chain"
        );
        if self.files.contains_key(path) {
            visiting.remove(path);
            return Ok(self.settings.contains(path));
        }
        ensure!(
            path.canonicalize()? == path,
            "Ruff configuration path is not canonical"
        );
        let (binding, bytes) = if path.starts_with(root) {
            candidate_inputs::repository_live(path, MAX_CONFIG)?
        } else {
            ensure!(
                grants.read_only.iter().any(|p| path.starts_with(p)),
                "external Ruff config lacks a read capability"
            );
            let sha = pins.get(path).context(
                "external ancestor/extended Ruff config requires --config-pin PATH=SHA256",
            )?;
            candidate_inputs::external_with_tmp(
                path,
                sha,
                MAX_CONFIG,
                root,
                Some(&grants.scratch_parent),
            )?
        };
        let text = std::str::from_utf8(&bytes).context("Ruff configuration must be UTF-8")?;
        let parsed: toml::Value =
            toml::from_str(text).context("invalid Ruff configuration TOML")?;
        let settings = if path.file_name().is_some_and(|s| s == "pyproject.toml") {
            parsed.get("tool").and_then(|v| v.get("ruff"))
        } else {
            Some(&parsed)
        };
        if !self.files.contains_key(path) {
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .context("configuration byte overflow")?;
            ensure!(
                self.bytes <= 8 * MAX_CONFIG && self.files.len() < MAX_CONFIGS,
                "Ruff configuration exceeds 8 MiB/1024 files"
            );
            self.files.insert(path.to_owned(), binding);
            self.digests
                .insert(path.to_owned(), conductor_native::syntax::fingerprint(text));
            if settings.is_some() {
                self.settings.insert(path.to_owned());
            }
        }
        if let Some(settings) = settings {
            reject_unsupported(settings)?;
            if let Some(extend) = settings.get("extend") {
                let relative = extend
                    .as_str()
                    .context("Ruff extend must be a literal string")?;
                ensure!(
                    !relative.contains(['$', '~']) && !relative.chars().any(char::is_control),
                    "Ruff extend interpolation is unsupported"
                );
                let parent = path.parent().context("configuration parent absent")?;
                // Resolve '..' once, then bind the exact canonical extension.
                let joined = parent.join(relative);
                let extended = normalize_absolute(&joined)?;
                ensure!(
                    joined
                        .canonicalize()
                        .context("resolve Ruff extended config")?
                        == extended,
                    "Ruff extend must not traverse symlinks"
                );
                self.add(&extended, root, grants, pins, visiting)?;
            }
        }
        visiting.remove(path);
        Ok(settings.is_some())
    }
    pub(crate) fn verify(&self) -> Result<()> {
        for input in self.files.values() {
            input.verify()?;
        }
        for path in &self.absent {
            match fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                _ => anyhow::bail!(
                    "Ruff configuration appeared or became inaccessible: {}",
                    path.display()
                ),
            }
        }
        Ok(())
    }
    pub(crate) fn evidence(&self) -> serde_json::Value {
        serde_json::json!({"files":self.digests,"absent":self.absent,
            "ambient_user_configuration":false,"supported_defaults":"ruff 0.16.5"})
    }
    pub(crate) fn paths(&self) -> Vec<&Path> {
        self.files.keys().map(PathBuf::as_path).collect()
    }
}
fn parse_pins(pins: &[String]) -> Result<BTreeMap<PathBuf, String>> {
    ensure!(pins.len() <= 128, "at most 128 external configuration pins");
    let mut values = BTreeMap::new();
    for pin in pins {
        let (path, hash) = pin
            .rsplit_once('=')
            .context("config pin must be PATH=SHA256")?;
        let path = PathBuf::from(path);
        ensure!(
            path.is_absolute() && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid config pin"
        );
        ensure!(
            values.insert(path, hash.to_owned()).is_none(),
            "duplicate config pin"
        );
    }
    Ok(values)
}
fn reject_unsupported(settings: &toml::Value) -> Result<()> {
    if let Some(extensions) = settings.get("extension") {
        for (extension, kind) in extensions
            .as_table()
            .context("Ruff extension must be a table")?
        {
            // Only selected source kinds matter; unknown extensions never become write scope.
            if extension == "py" || extension == "pyi" {
                ensure!(
                    matches!(kind.as_str(), Some("python" | "pyi")),
                    "selected Ruff extension must remain Python/pyi"
                );
            }
        }
    }
    // Ruff expands these path-bearing settings against environment. Admit only
    // literal settings because the child deliberately receives a private HOME.
    for key in [
        "src",
        "namespace-packages",
        "exclude",
        "extend-exclude",
        "include",
        "extend-include",
    ] {
        if let Some(value) = settings.get(key) {
            literal_paths(value)?;
        }
    }
    Ok(())
}
fn literal_paths(value: &toml::Value) -> Result<()> {
    match value {
        toml::Value::String(text) => ensure!(
            !text.contains(['$', '~']),
            "Ruff path interpolation is unsupported"
        ),
        toml::Value::Array(values) => {
            for value in values {
                literal_paths(value)?;
            }
        }
        _ => (), // Ruff validates schema itself before producing candidate bytes.
    }
    Ok(())
}
#[cfg(test)]
pub(super) fn test_settings(text: &str) -> Result<()> {
    reject_unsupported(&toml::from_str::<toml::Value>(text)?)
}

fn normalize_absolute(path: &Path) -> Result<PathBuf> {
    use std::path::Component;
    ensure!(path.is_absolute(), "configuration must be absolute");
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::RootDir => out.push("/"),
            Component::Normal(name) => out.push(name),
            Component::CurDir => (),
            Component::ParentDir => ensure!(out.pop(), "configuration escapes filesystem root"),
            Component::Prefix(_) => anyhow::bail!("unsupported configuration prefix"),
        }
    }
    Ok(out)
}
