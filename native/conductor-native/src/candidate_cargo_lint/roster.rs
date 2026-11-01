use super::{inventory::Pattern, path_parts};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    crates: Crates,
    globs: Globs,
    #[serde(default)]
    prerequisites: BTreeMap<String, Prerequisite>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Crates {
    #[serde(default)]
    tested: Vec<String>,
    #[serde(default)]
    linted: Vec<String>,
    #[serde(default)]
    unstyled: Vec<String>,
    #[serde(default)]
    excluded: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Globs {
    manifests: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Prerequisite {
    #[serde(default)]
    pub artifact: Option<String>,
    #[serde(default = "default_command")]
    pub command: String,
    #[serde(default = "default_reason")]
    pub reason: String,
}
fn default_command() -> String {
    "the documented command".into()
}
fn default_reason() -> String {
    "a build prerequisite is missing".into()
}
pub struct Roster {
    pub(super) tested: BTreeSet<String>,
    pub(super) linted: BTreeSet<String>,
    pub(super) unstyled: BTreeSet<String>,
    pub(super) excluded: BTreeSet<String>,
    pub(super) patterns: Vec<Pattern>,
    pub(super) prerequisites: BTreeMap<String, Prerequisite>,
}
impl Roster {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 1024 * 1024 {
            return Err("lint roster exceeds 1 MiB".into());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| "lint roster must be UTF-8")?;
        let mut raw: Raw = toml::from_str(text).map_err(|_| "invalid typed lint roster TOML")?;
        if raw.globs.manifests.is_empty() || raw.globs.manifests.len() > 128 {
            return Err("lint roster requires 1..128 manifest globs".into());
        }
        let patterns = raw
            .globs
            .manifests
            .iter()
            .map(|s| Pattern::new(s))
            .collect::<Result<Vec<_>, _>>()?;
        let count = raw
            .crates
            .tested
            .len()
            .checked_add(raw.crates.linted.len())
            .and_then(|n| n.checked_add(raw.crates.unstyled.len()))
            .and_then(|n| n.checked_add(raw.crates.excluded.len()))
            .and_then(|n| n.checked_add(raw.prerequisites.len()))
            .ok_or("lint roster row overflow")?;
        if count > 10_000 {
            return Err("lint roster entries exceed 10000".into());
        }
        for (key, value) in &mut raw.prerequisites {
            path_parts(key, true)?;
            if value.command.len() > 4096
                || value.reason.len() > 4096
                || value.command.contains('\0')
                || value.reason.contains('\0')
            {
                return Err("lint prerequisite display metadata exceeds bounds".into());
            }
            if value.artifact.as_deref() == Some("") {
                value.artifact = None;
            }
            if let Some(artifact) = &value.artifact {
                path_parts(artifact, false)?;
            }
        }
        Ok(Self {
            tested: paths(raw.crates.tested)?,
            linted: paths(raw.crates.linted)?,
            unstyled: paths(raw.crates.unstyled)?,
            excluded: paths(raw.crates.excluded)?,
            patterns,
            prerequisites: raw.prerequisites,
        })
    }
}
fn paths(values: Vec<String>) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    for value in values {
        path_parts(&value, true)?;
        if !out.insert(value) {
            return Err("duplicate lint roster crate path".into());
        }
    }
    Ok(out)
}
