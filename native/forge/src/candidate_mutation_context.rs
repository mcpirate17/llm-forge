//! Separate native-verified originals; never union classifications into a receipt.
use super::*;
use conductor_native::mutation_evidence::CapturedAttempt;
pub(super) fn declarations(
    config: Option<&conductor_native::project_config::ProjectConfig>,
    gated: &Value,
) -> Result<BTreeMap<String, Vec<String>>> {
    let raw = config.and_then(|c| c.conductor.get("mutation_value_contexts"));
    let mut result = BTreeMap::new();
    let Some(raw) = raw else { return Ok(result) };
    let table = raw
        .as_table()
        .context("mutation_value_contexts must be a nodeid/source-path table")?;
    for nodes in gated
        .as_object()
        .context("gated nodeids must be an object")?
        .values()
    {
        for node in strings(nodes)? {
            let Some(raw) = table.get(&node) else {
                continue;
            };
            let sources = raw
                .as_array()
                .filter(|v| !v.is_empty())
                .context("source contexts must be a nonempty list")?;
            let mut paths = BTreeSet::new();
            for source in sources {
                let path = source
                    .as_str()
                    .context("source context path must be a string")?;
                paths.insert(
                    conductor_native::mutation_evidence::normalize_path(path)
                        .map_err(anyhow::Error::msg)?,
                );
            }
            result.insert(node, paths.into_iter().collect());
        }
    }
    Ok(result)
}
pub(super) fn complete(receipt: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if receipt["status"] != "PASS" {
        errors.push("attempt is not PASS".into());
    }
    if receipt["baseline"]["returncode"].as_i64() != Some(0)
        || receipt["baseline"]["timed_out"] != false
    {
        errors.push("baseline did not complete successfully".into());
    }
    for name in ["SURVIVED", "TIMED_OUT", "ERROR", "UNVIABLE", "NO_COVERAGE"] {
        if receipt["outcome_counts"][name].as_u64() != Some(0) {
            errors.push(format!("incomplete outcome count {name}"));
        }
    }
    let killed = receipt["outcome_counts"]["KILLED"].as_u64();
    if !killed.is_some_and(|v| v > 0)
        || receipt["attribution"]["status"] != "ATTRIBUTED"
        || receipt["attribution"]["attributed_mutants"].as_u64() != killed
        || receipt["attribution"]["killed_mutants"].as_u64() != killed
    {
        errors.push("reported kills lack complete attribution".into());
    }
    if receipt["test_value"]["status"] != "PASS" {
        errors.push("current PASS test-value evidence is missing".into());
    }
    errors
}
pub(super) fn errors(
    node: &str,
    sources: &[String],
    attempts: &[CapturedAttempt],
) -> Result<Vec<String>> {
    let mut groups: BTreeMap<String, Vec<&CapturedAttempt>> = BTreeMap::new();
    for attempt in attempts.iter().filter(|a| a.bindings_current) {
        let mut context = Map::new();
        for key in [
            "campaign_id",
            "manifest_sha256",
            "source_sha256",
            "test_sha256",
            "runner_sha256",
            "runner_components_sha256",
        ] {
            context.insert(key.into(), attempt.expanded[key].clone());
        }
        groups
            .entry(crate::candidate_test_selection::digest_json(&context)?)
            .or_default()
            .push(attempt);
    }
    let mut accepted = BTreeSet::new();
    let mut debt = BTreeSet::new();
    for group in groups.values() {
        match latest(group, node) {
            Err(error) => {
                debt.insert(error.to_string());
            }
            Ok(latest) => {
                for attempt in latest {
                    let errors = conductor_native::mutation_value_admission::admission_errors(
                        &attempt.expanded["test_value"],
                        &[node.into()],
                    )
                    .map_err(anyhow::Error::msg)?;
                    if !errors.is_empty() {
                        debt.extend(errors);
                        continue;
                    }
                    for source in sources {
                        if source_kill(&attempt.expanded, node, source) {
                            accepted.insert(source.clone());
                        }
                    }
                }
            }
        }
    }
    let reason = if debt.is_empty() {
        "no positive current complete witness".into()
    } else {
        debt.into_iter().collect::<Vec<_>>().join("; ")
    };
    let width = reason
        .len()
        .saturating_add(node.len())
        .saturating_add(sources.iter().map(String::len).max().unwrap_or(0))
        .saturating_add(128);
    ensure!(
        width.saturating_mul(sources.len()) <= 32 * 1024 * 1024,
        "contextual finding amplification exceeds32MiB"
    );
    Ok(sources
        .iter()
        .filter(|s| !accepted.contains(*s))
        .map(|source| {
            format!(
                "new test {} lacks admitted source context {}: {reason}",
                repr(node),
                repr(source)
            )
        })
        .collect())
}
fn latest<'a>(attempts: &[&'a CapturedAttempt], node: &str) -> Result<Vec<&'a CapturedAttempt>> {
    let mut stamped = Vec::new();
    let mut signatures = BTreeSet::new();
    for attempt in attempts {
        let text = attempt.expanded["generated_at"]
            .as_str()
            .context("attempt timestamp is missing")?;
        crate::candidate_policy::validate_today(
            text.get(..10).context("invalid attempt timestamp")?,
        )?;
        let stamp = crate::instant::parse(text)
            .filter(|s| s.is_finite())
            .context("attempt timestamp is invalid or has no timezone")?;
        stamped.push((stamp, *attempt));
        if attempt.valid && complete(&attempt.expanded).is_empty() {
            let rows: Vec<_> = attempt.expanded["test_value"]["tests"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| r["nodeid"].as_str() == Some(node))
                .collect();
            signatures.insert(crate::candidate_test_selection::digest_json(&rows)?);
        }
    }
    ensure!(
        signatures.len() <= 1,
        "conflicting classifications in the same final context"
    );
    let stamp = stamped
        .iter()
        .map(|(s, _)| *s)
        .max_by(f64::total_cmp)
        .context("no context attempts")?;
    let latest: Vec<_> = stamped
        .into_iter()
        .filter(|(s, _)| *s == stamp)
        .map(|(_, a)| a)
        .collect();
    ensure!(
        latest
            .iter()
            .all(|a| a.valid && complete(&a.expanded).is_empty()),
        "latest attempt in required context is incomplete or failed native verification"
    );
    Ok(latest)
}
fn source_kill(receipt: &Value, node: &str, source: &str) -> bool {
    if receipt["source_sha256"].get(source).is_none() {
        return false;
    }
    let kills: BTreeSet<_> = receipt["test_value"]["tests"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["nodeid"].as_str() == Some(node))
        .flat_map(|r| r["killed_mutants"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .collect();
    receipt["mutants"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|r| {
            r["path"].as_str() == Some(source)
                && r["outcome"] == "KILLED"
                && r["id"].as_str().is_some_and(|id| kills.contains(id))
        })
}
pub(super) fn repr(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}
