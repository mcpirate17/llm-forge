//! Runtime contract registry parser shared with the optional embedded carrier.
//! This module embeds no legacy registry bytes.
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) const TEST_DIR: &str = "native/conductor-native/tests";
pub(crate) fn native_source(path: &str) -> bool {
    ["native/conductor-native/src/", "native/slop-core/src/"]
        .iter()
        .any(|prefix| {
            path.strip_prefix(*prefix)
                .is_some_and(|name| name.ends_with(".rs") && !name.contains('/'))
        })
        || path
            .strip_prefix("native/forge/src/")
            .is_some_and(|name| name.ends_with(".rs"))
}

pub(crate) fn fixture_source(path: &str) -> bool {
    path.starts_with(&format!("{TEST_DIR}/fixtures/")) && path.ends_with(".rs")
}

pub(crate) fn corpus_source(path: &str) -> bool {
    path.ends_with(".json")
}

pub(crate) fn patch_fixture_source(path: &str) -> bool {
    path.starts_with("src/conductor/testdata/") && path.ends_with(".patch")
}

pub(crate) fn python_fixture_source(path: &str) -> bool {
    (path.starts_with("src/conductor/testdata/")
        || path.starts_with(&format!("{TEST_DIR}/fixtures/")))
        && path.ends_with(".py")
}

#[derive(Default)]
pub struct Registry {
    pub(crate) by_source: BTreeMap<String, BTreeSet<String>>,
    pub(crate) targets: BTreeSet<String>,
}

impl Registry {
    pub fn targets(&self) -> impl Iterator<Item = &str> {
        self.targets.iter().map(String::as_str)
    }
    pub fn targets_for(&self, source: &str) -> impl Iterator<Item = &str> {
        self.by_source
            .get(source)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }
}

pub fn could_map(source: &str) -> bool {
    let source_file = (source.starts_with("src/")
        && !python_fixture_source(source)
        && [".py", ".json", ".sh", ".toml"]
            .iter()
            .any(|suffix| source.ends_with(suffix)))
        || source == ".claude/hooks/dispatch.py";
    let helper = source.starts_with(&format!("{TEST_DIR}/python_contracts/"))
        && source.ends_with(".rs")
        && !source
            .trim_start_matches(&format!("{TEST_DIR}/python_contracts/"))
            .contains('/');
    source_file
        || helper
        || native_source(source)
        || fixture_source(source)
        || corpus_source(source)
        || patch_fixture_source(source)
        || python_fixture_source(source)
}

pub fn parse_registry(contents: &str) -> Result<Registry> {
    anyhow::ensure!(
        contents.len() <= 4 * 1024 * 1024,
        "contract registry exceeds byte limit"
    );
    let mut entries = Registry::default();
    for (index, line) in contents.lines().enumerate() {
        anyhow::ensure!(
            index < 100_000 && line.len() <= 8192,
            "contract registry row bound exceeded"
        );
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (source, target) = line
            .split_once('\t')
            .with_context(|| format!("contract registry line {} has no tab", index + 1))?;
        if !could_map(source)
            || source.contains("..")
            || source.contains('\\')
            || source.chars().any(char::is_control)
            || source.split('/').any(|part| part.is_empty() || part == ".")
        {
            bail!(
                "invalid contract path on registry line {}: {source}",
                index + 1
            );
        }
        if !target.starts_with("python_contracts_")
            || !target
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            bail!(
                "invalid contract target on registry line {}: {target}",
                index + 1
            );
        }
        if !entries
            .by_source
            .entry(source.to_owned())
            .or_default()
            .insert(target.to_owned())
        {
            bail!("duplicate contract mapping on registry line {}", index + 1);
        }
        entries.targets.insert(target.to_owned());
    }
    if entries.targets.is_empty() {
        bail!("contract registry is empty");
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supplied_registry_is_bounded_and_rejects_duplicate_or_unsafe_rows() {
        let registry =
            parse_registry("src/a.py\tpython_contracts_a\nsrc/a.py\tpython_contracts_b\n").unwrap();
        assert_eq!(
            registry.targets_for("src/a.py").collect::<Vec<_>>(),
            ["python_contracts_a", "python_contracts_b"]
        );
        assert!(
            parse_registry("src/a.py\tpython_contracts_a\nsrc/a.py\tpython_contracts_a\n").is_err()
        );
        assert!(parse_registry("../escape.py\tpython_contracts_a\n").is_err());
        assert!(parse_registry(&"x".repeat(4 * 1024 * 1024 + 1)).is_err());
    }
}
