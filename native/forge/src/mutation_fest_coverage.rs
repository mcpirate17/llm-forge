//! Validate pytest-cov contexts and curate only pinned test nodes for Fest.
use crate::{candidate_inputs::BoundFile, mutation_campaign::Campaign};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Map, Value};
use std::{collections::BTreeSet, fs, io::Write, path::Path};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONTEXTS: usize = 1_000_000;

fn source_path<'a>(name: &'a str, root: &Path) -> Result<&'a str> {
    let path = Path::new(name);
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .context("coverage path outside snapshot")?
    } else {
        path
    };
    let relative = relative.to_str().context("coverage path is not UTF-8")?;
    conductor_native::mutation_snapshot_inputs::validate_path(relative)
        .map_err(anyhow::Error::msg)?;
    let expected = root.join(relative);
    if path.is_absolute() {
        ensure!(
            expected.as_os_str() == path.as_os_str(),
            "coverage path is not canonical"
        );
    }
    ensure!(
        expected.canonicalize()?.as_os_str() == expected.as_os_str(),
        "coverage file is aliased or missing"
    );
    Ok(relative)
}

fn selected_node<'a>(context: &'a str, tests: &BTreeSet<&str>) -> Result<Option<&'a str>> {
    if context.is_empty() {
        return Ok(None);
    }
    let (node, phase) = context
        .rsplit_once('|')
        .context("coverage context lacks pytest phase")?;
    ensure!(
        matches!(phase, "setup" | "run" | "teardown")
            && !node.contains('|')
            && !node.chars().any(char::is_control),
        "unsupported pytest coverage context"
    );
    let (file, item) = node
        .split_once("::")
        .context("coverage context lacks a test node")?;
    ensure!(
        !item.is_empty() && tests.contains(file),
        "coverage includes an unselected test context"
    );
    Ok(Some(node))
}

/// Capture the raw report and emit a private, normalized Fest-only projection.
/// Both bindings must be reverified after the engine returns.
pub(crate) fn capture(
    raw: &Path,
    output: &Path,
    root: &Path,
    campaign: &Campaign,
) -> Result<(BoundFile, BoundFile)> {
    let (held, bytes) = crate::candidate_inputs::repository_live(raw, MAX_BYTES)?;
    let value =
        crate::tool_schema::parse_document_with_limit(std::str::from_utf8(&bytes)?, MAX_BYTES)?;
    let curated = curate(&value, root, campaign)?;
    let bytes = serde_json::to_vec(&curated)?;
    ensure!(
        bytes.len() <= MAX_BYTES,
        "curated Fest coverage exceeds bound"
    );
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    let (projected, reread) = crate::candidate_inputs::repository_live(output, MAX_BYTES)?;
    ensure!(
        reread == bytes,
        "curated Fest coverage changed during creation"
    );
    held.verify()?;
    projected.verify()?;
    Ok((held, projected))
}

fn curate(value: &Value, root: &Path, campaign: &Campaign) -> Result<Value> {
    ensure!(
        value["meta"]["show_contexts"] == true,
        "coverage JSON must include test contexts"
    );
    let files = value["files"]
        .as_object()
        .context("coverage JSON files missing")?;
    ensure!(
        !files.is_empty() && files.len() <= 100_000,
        "invalid coverage file count"
    );
    let tests: BTreeSet<_> = campaign.test_sha256.keys().map(String::as_str).collect();
    let mut observed = 0usize;
    let mut selected_contexts = 0usize;
    let mut output = Map::new();
    for (name, file) in files {
        let relative = source_path(name, root)?;
        let selected = campaign.source_sha256.contains_key(relative);
        let contexts = file["contexts"]
            .as_object()
            .context("coverage line contexts missing")?;
        let mut lines = Map::new();
        for (line, entries) in contexts {
            ensure!(
                line.parse::<u32>().is_ok_and(|n| n > 0),
                "invalid coverage line number"
            );
            let mut nodes = BTreeSet::new();
            for entry in entries
                .as_array()
                .context("invalid coverage context list")?
            {
                let context = entry.as_str().context("invalid coverage context")?;
                observed = observed
                    .checked_add(1)
                    .context("coverage context overflow")?;
                ensure!(
                    observed <= MAX_CONTEXTS && context.len() <= 4096,
                    "coverage context exceeds bound"
                );
                if let Some(node) = selected_node(context, &tests)? {
                    if selected {
                        selected_contexts += 1;
                        nodes.insert(node.to_owned());
                    }
                }
            }
            if selected && !nodes.is_empty() {
                lines.insert(line.clone(), json!(nodes));
            }
        }
        if selected {
            let key = root
                .join(relative)
                .to_str()
                .context("selected coverage path is not UTF-8")?
                .to_owned();
            ensure!(
                output.insert(key, json!({"contexts":lines})).is_none(),
                "duplicate selected coverage path"
            );
        }
    }
    ensure!(
        output.len() == campaign.source_sha256.len() && selected_contexts > 0,
        "coverage lacks selected source/test contexts"
    );
    Ok(json!({"files":output}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contexts_require_pinned_nodes_and_strip_only_known_phases() {
        let tests = ["tests/test_subject.py"].into_iter().collect();
        assert_eq!(
            selected_node("tests/test_subject.py::test_a|run", &tests).unwrap(),
            Some("tests/test_subject.py::test_a")
        );
        assert_eq!(selected_node("", &tests).unwrap(), None);
        assert!(selected_node("tests/other.py::test_a|run", &tests).is_err());
        assert!(selected_node("tests/test_subject.py::test_a|other", &tests).is_err());
        assert!(selected_node("tests/test_subject.py::test_a|run|setup", &tests).is_err());
    }
}

#[cfg(test)]
#[path = "mutation_fest_coverage_tests.rs"]
mod admission_tests;
