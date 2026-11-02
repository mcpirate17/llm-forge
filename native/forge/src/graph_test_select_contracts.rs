//! Runtime-only advisory contract registry. No embedded compatibility corpus.
use anyhow::{ensure, Context, Result};
use conductor_native::contract_registry::{could_map, parse_registry};
use serde::Serialize;
use std::{collections::BTreeSet, fs, io::Read, path::Path};
const REGISTRY: &str = "native/conductor-native/src/python_contract_targets.tsv";
const EXTRA: &str = "native/conductor-native/src/python_contract_targets_extra.tsv";

#[derive(Debug, Default, Serialize)]
pub(super) struct Contracts {
    pub targets: Vec<String>,
    pub test_paths: Vec<String>,
}

fn read(root: &Path, relative: &str) -> Result<String> {
    conductor_native::regular_file::require_regular_file(root, relative)?;
    let mut text = String::new();
    fs::File::open(root.join(relative))?
        .take(4 * 1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    ensure!(
        text.len() <= 4 * 1024 * 1024,
        "contract registry exceeds 4 MiB"
    );
    Ok(text)
}

/// Exact mapped Python changes select their contracts. Native/helper changes
/// conservatively select all contracts; this is broader than the legacy planner.
/// Incomplete graph evidence also selects every registry target.
pub(super) fn plan(root: &Path, sources: &[String], complete: bool) -> Result<Contracts> {
    if sources.is_empty() {
        return Ok(Contracts::default());
    }
    if !root.join(REGISTRY).exists() {
        ensure!(
            !root.join(EXTRA).exists(),
            "orphaned contract registry extension"
        );
        ensure!(
            !root.join("native/conductor-native/Cargo.toml").exists(),
            "native contract registry is missing"
        );
        return Ok(Contracts::default());
    }
    let mut text = read(root, REGISTRY)?;
    if text
        .lines()
        .any(|line| line == "# include: python_contract_targets_extra.tsv")
    {
        text.push('\n');
        text.push_str(&read(root, EXTRA)?);
    } else {
        ensure!(
            !sources.iter().any(|path| path == EXTRA),
            "changed registry extension is not declared"
        );
    }
    let registry = parse_registry(&text)?;
    let all = !complete
        || sources.iter().any(|path| {
            path == REGISTRY
                || path == EXTRA
                || (path.starts_with("native/conductor-native/") && path.ends_with(".rs"))
                || (could_map(path)
                    && (path.ends_with(".rs") || path.contains("/python_contracts/")))
        });
    let targets = if all {
        registry
            .targets()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
    } else {
        sources
            .iter()
            .flat_map(|path| registry.targets_for(path).map(str::to_owned))
            .collect()
    };
    let mut paths = Vec::new();
    for target in &targets {
        let relative = format!("native/conductor-native/tests/{target}.rs");
        conductor_native::regular_file::require_regular_file(root, &relative)
            .with_context(|| format!("selected contract target unavailable: {target}"))?;
        paths.push(relative);
    }
    Ok(Contracts {
        targets: targets.into_iter().collect(),
        test_paths: paths,
    })
}
