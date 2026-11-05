//! Per-selected-file scored-mutant evidence for every native mutation engine.
use crate::mutation_campaign::{Campaign, Step, Stop};
use crate::mutation_run_scope::targets_of;
use crate::mutation_score::{rows, Receipt, INCOMPLETE};
use conductor_native::mutation_source_coverage as shared;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::Path;

fn confined(root: &Path, raw: &str) -> Result<String, String> {
    let normal = shared::normalise(raw)?;
    let resolved = root
        .join(&normal)
        .canonicalize()
        .map_err(|error| format!("cannot resolve mutant source {raw:?}: {error}"))?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err(format!(
            "mutant source is outside snapshot or not a file: {raw:?}"
        ));
    }
    let relative = resolved
        .strip_prefix(root)
        .map_err(|error| error.to_string())?;
    if relative != Path::new(&normal) {
        return Err(format!("symlink source aliases are unsupported: {raw:?}"));
    }
    Ok(normal)
}

fn measure(campaign: &Campaign, worktree: &Path, receipt: &Receipt) -> Result<Value, String> {
    let root = worktree.canonicalize().map_err(|error| error.to_string())?;
    let targets = targets_of(campaign, &root)?;
    let selected = targets
        .iter()
        .map(|target| confined(&root, target))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let pins = campaign
        .source_sha256
        .keys()
        .map(|pin| confined(&root, pin))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if selected != pins {
        return Err("selected mutation targets and source pins differ".into());
    }
    let mut checked = BTreeSet::new();
    for row in rows(receipt) {
        let raw = row
            .get("path")
            .and_then(Value::as_str)
            .ok_or("mutant source path is missing")?;
        let normal = shared::normalise(raw)?;
        if checked.insert(normal.clone()) {
            confined(&root, &normal)?;
        }
    }
    let pins: Map<String, Value> = campaign
        .source_sha256
        .iter()
        .map(|(path, hash)| (path.clone(), json!(hash)))
        .collect();
    shared::report(&pins, rows(receipt))
}

/// Refuse an unmeasured selected file; rows are retained for accounting.
pub(crate) fn require(campaign: &Campaign, worktree: &Path, receipt: &mut Receipt) -> Step<()> {
    match measure(campaign, worktree, receipt) {
        Ok(report) if report["complete"] == true => {
            receipt.insert("source_coverage".into(), report);
            Ok(())
        }
        Ok(report) => {
            let missing = report["missing"].clone();
            receipt.insert("source_coverage".into(), report);
            receipt.insert("status".into(), json!(INCOMPLETE));
            Err(Stop::Refused(format!(
                "selected mutation sources have no scored mutants: {missing}"
            )))
        }
        Err(error) => {
            receipt.insert(
                "source_coverage".into(),
                json!({"complete": false, "error": error}),
            );
            receipt.insert("status".into(), json!(INCOMPLETE));
            Err(Stop::Refused(error))
        }
    }
}

#[cfg(test)]
#[path = "mutation_source_coverage_tests.rs"]
mod tests;
