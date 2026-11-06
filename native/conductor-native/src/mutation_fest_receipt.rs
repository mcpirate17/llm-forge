//! Confined Fest's additional owned cache cleanup evidence.
//! Paths are diagnostics, never authority to reopen or remove a recorded resource.
use serde_json::{Map, Value};

pub fn cleanup_error(payload: &Map<String, Value>) -> Option<String> {
    let Some(cache) = payload.get("fest_cache").and_then(Value::as_object) else {
        return Some("confined Fest bytecode cache evidence is missing".into());
    };
    let path = cache.get("path").and_then(Value::as_str);
    if cache.get("schema_version").and_then(Value::as_u64) != Some(1)
        || ["read_only", "initially_empty", "removed"]
            .iter()
            .any(|key| cache.get(*key) != Some(&Value::Bool(true)))
        || !path.is_some_and(|path| {
            path.starts_with('/')
                && path.len() <= 4096
                && !path.bytes().any(|b| b.is_ascii_control())
        })
    {
        return Some("confined Fest bytecode cache isolation or cleanup is incomplete".into());
    }
    None
}

/// Verify cooperative JUnit attribution against the independently verified campaign.
pub fn attribution_error(
    payload: &Map<String, Value>,
    campaign_id: &str,
    source_pins: &Value,
    test_pins: &Value,
) -> Option<String> {
    check_attribution(payload, campaign_id, source_pins, test_pins).err()
}

fn killed_rows(
    payload: &Map<String, Value>,
    source_pins: &Value,
) -> Result<(Vec<String>, Vec<String>), String> {
    let sources = source_pins.as_object().ok_or("missing source pins")?;
    let rows = payload
        .get("mutants")
        .and_then(Value::as_array)
        .ok_or("missing mutant rows")?;
    if rows.len() > 100_000 {
        return Err("too many mutant rows".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut killed = std::collections::BTreeSet::new();
    let mut paths = std::collections::BTreeSet::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 4096)
            .ok_or("invalid mutant id")?;
        if !ids.insert(id.to_owned()) {
            return Err("duplicate mutant id".into());
        }
        if row.get("outcome").and_then(Value::as_str) == Some("KILLED") {
            let path = row
                .get("path")
                .and_then(Value::as_str)
                .ok_or("missing killed mutant path")?;
            if !sources.contains_key(path) {
                return Err("killed mutant outside campaign sources".into());
            }
            killed.insert(id.to_owned());
            paths.insert(path.to_owned());
        }
    }
    Ok((killed.into_iter().collect(), paths.into_iter().collect()))
}

fn check_report(
    report: &Value,
    allowed: &std::collections::BTreeSet<String>,
    baseline: bool,
) -> Result<(), String> {
    let tests = report
        .get("tests")
        .and_then(Value::as_object)
        .ok_or("missing JUnit tests")?;
    if report.get("status").and_then(Value::as_str) != Some("COMPLETE")
        || tests.is_empty()
        || tests.len() > 4096
    {
        return Err("incomplete JUnit attribution report".into());
    }
    for key in ["missing_nodeids", "unmapped_cases"] {
        if !report
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            return Err(format!("JUnit attribution has {key}"));
        }
    }
    let mut failed = std::collections::BTreeSet::new();
    for (node, test) in tests {
        if !allowed.contains(node) {
            return Err("JUnit test outside selected campaign".into());
        }
        match test.get("outcome").and_then(Value::as_str) {
            Some("PASSED") => (),
            Some("FAILED") if !baseline => {
                failed.insert(node.clone());
            }
            _ => return Err("JUnit attribution contains error or unsupported outcome".into()),
        }
    }
    let listed = report
        .get("failed_nodeids")
        .and_then(Value::as_array)
        .ok_or("missing failed node IDs")?;
    let mut observed = std::collections::BTreeSet::new();
    for item in listed {
        if !observed.insert(item.as_str().ok_or("invalid failed node ID")?.to_owned()) {
            return Err("duplicate failed node ID".into());
        }
    }
    if observed != failed
        || (!baseline && failed.is_empty())
        || (baseline && tests.len() != allowed.len())
    {
        return Err("JUnit failed node IDs disagree with selected test outcomes".into());
    }
    Ok(())
}

fn check_attribution(
    payload: &Map<String, Value>,
    campaign_id: &str,
    source_pins: &Value,
    test_pins: &Value,
) -> Result<(), String> {
    let (ids, paths) = killed_rows(payload, source_pins)?;
    if ids.is_empty() {
        return Ok(());
    }
    let evidence = payload
        .get("fest_attribution_observations")
        .ok_or("missing Fest attribution observations")?;
    crate::candidate_cargo_audit::serialized_size(evidence, 4 * 1024 * 1024)
        .map_err(|_| "Fest attribution observations exceed 4 MiB".to_owned())?;
    let matrix = evidence
        .get("mutants")
        .and_then(Value::as_object)
        .ok_or("missing JUnit matrix")?;
    let baselines = evidence
        .get("baseline_reports")
        .and_then(Value::as_array)
        .ok_or("missing JUnit baselines")?;
    let attribution = payload
        .get("attribution")
        .ok_or("missing attribution summary")?;
    if evidence.get("schema_version").and_then(Value::as_u64) != Some(1)
        || evidence.get("baseline_exit_codes") != Some(&serde_json::json!([0, 0]))
        || baselines.len() != 2
        || attribution.get("status").and_then(Value::as_str) != Some("ATTRIBUTED")
        || attribution.get("attributed_ids") != Some(&serde_json::json!(ids))
        || attribution.get("unattributed") != Some(&serde_json::json!([]))
        || attribution.get("killed_mutants").and_then(Value::as_u64) != Some(ids.len() as u64)
        || attribution
            .get("attributed_mutants")
            .and_then(Value::as_u64)
            != Some(ids.len() as u64)
        || matrix
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            != ids.iter().cloned().collect()
    {
        return Err("Fest attribution is incomplete or differs from killed mutants".into());
    }
    let exits = evidence
        .get("mutant_exit_codes")
        .and_then(Value::as_object)
        .ok_or("missing attribution exit codes")?;
    if exits.len() != ids.len()
        || ids
            .iter()
            .any(|id| exits.get(id) != Some(&serde_json::json!(1)))
    {
        return Err("attribution did not exit with normal pytest failures".into());
    }
    let tests = test_pins
        .as_object()
        .ok_or("missing independently pinned tests")?;
    let selected: std::collections::BTreeSet<String> = baselines[0]
        .get("tests")
        .and_then(Value::as_object)
        .ok_or("missing baseline tests")?
        .keys()
        .cloned()
        .collect();
    if selected.iter().any(|node| {
        node.len() > 4096
            || !node
                .split_once("::")
                .is_some_and(|(path, name)| !name.is_empty() && tests.contains_key(path))
    }) {
        return Err("attribution node outside pinned tests".into());
    }
    for report in baselines {
        check_report(report, &selected, true)?;
    }
    for report in matrix.values() {
        check_report(report, &selected, false)?;
    }
    compare_analysis(
        payload,
        campaign_id,
        &ids,
        &paths,
        baselines,
        matrix,
        selected,
    )
}

fn compare_analysis(
    payload: &Map<String, Value>,
    campaign_id: &str,
    ids: &[String],
    paths: &[String],
    baselines: &[Value],
    matrix: &Map<String, Value>,
    selected: std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let ranked: Vec<String> = selected.into_iter().collect();
    let spec = serde_json::json!({"adapter":"pytest-junit","baseline_repetitions":2,"contracts":[{"id":campaign_id,"criticality":"high","active_paths":paths}],"tests":ranked.iter().map(|nodeid| serde_json::json!({"nodeid":nodeid,"contract_id":campaign_id,"intentional_redundancy":false})).collect::<Vec<_>>(),"mutation_contracts":ids.iter().map(|id| serde_json::json!([id,campaign_id])).collect::<Vec<_>>()});
    let outcomes: Map<String, Value> = ids
        .iter()
        .map(|id| (id.clone(), serde_json::json!("KILLED")))
        .collect();
    let analysis = crate::mutation_value::analyze_reports(
        spec,
        baselines.to_vec(),
        &Value::Object(matrix.clone()),
        &Value::Object(outcomes),
    )?;
    let analysis: Value = serde_json::from_str(&analysis).map_err(|e| e.to_string())?;
    if analysis.get("status").and_then(Value::as_str) != Some("PASS")
        || payload.get("test_value") != Some(&analysis)
    {
        return Err("Fest test-value analysis disagrees with JUnit observations".into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod attribution_tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn fixture() -> Value {
        let node = "tests/test_a.py::test_a";
        let baseline = json!({"status":"COMPLETE","tests":{node:{"outcome":"PASSED","duration_seconds":0.1,"cases":1}},"failed_nodeids":[],"missing_nodeids":[],"unmapped_cases":[]});
        let mutant = json!({"status":"COMPLETE","tests":{node:{"outcome":"FAILED","duration_seconds":0.1,"cases":1}},"failed_nodeids":[node],"missing_nodeids":[],"unmapped_cases":[]});
        let spec = json!({"adapter":"pytest-junit","baseline_repetitions":2,"contracts":[{"id":"c","criticality":"high","active_paths":["src/a.py"]}],"tests":[{"nodeid":node,"contract_id":"c","intentional_redundancy":false}],"mutation_contracts":[["m","c"]]});
        let baselines = vec![baseline.clone(), baseline];
        let matrix = json!({"m":mutant});
        let analysis = crate::mutation_value::analyze_reports(
            spec,
            baselines.clone(),
            &matrix,
            &json!({"m":"KILLED"}),
        )
        .unwrap();
        json!({"mutants":[{"id":"m","path":"src/a.py","outcome":"KILLED"}],"attribution":{"status":"ATTRIBUTED","attributed_ids":["m"],"unattributed":[],"killed_mutants":1,"attributed_mutants":1},"fest_attribution_observations":{"schema_version":1,"baseline_reports":baselines,"mutants":matrix,"baseline_exit_codes":[0,0],"mutant_exit_codes":{"m":1}},"test_value":serde_json::from_str::<Value>(&analysis).unwrap()})
    }
    fn check(value: &Value) -> Option<String> {
        attribution_error(
            value.as_object().unwrap(),
            "c",
            &json!({"src/a.py":"hash"}),
            &json!({"tests/test_a.py":"hash"}),
        )
    }
    #[test]
    fn setup_errors_missing_xml_and_partial_kills_never_authorize_pass() {
        let valid = fixture();
        assert_eq!(check(&valid), None);
        for change in ["setup", "exit", "missing", "partial", "mismatch", "outside"] {
            let mut bad = valid.clone();
            match change {
                "setup" => {
                    bad["fest_attribution_observations"]["mutants"]["m"]["tests"]
                        ["tests/test_a.py::test_a"]["outcome"] = json!("ERROR")
                }
                "exit" => bad["fest_attribution_observations"]["mutant_exit_codes"]["m"] = json!(2),
                "missing" => {
                    bad.as_object_mut()
                        .unwrap()
                        .remove("fest_attribution_observations");
                }
                "partial" => {
                    bad["attribution"]["unattributed"] = json!([{"id":"m","reason":"no XML"}])
                }
                "mismatch" => bad["test_value"]["status"] = json!("PASS-FAKED"),
                "outside" => bad["mutants"][0]["path"] = json!("other.py"),
                _ => unreachable!(),
            }
            assert!(check(&bad).is_some(), "{change}");
        }
    }

    #[test]
    fn observation_limit_counts_escaped_json_bytes_at_the_exact_boundary() {
        const LIMIT: usize = 4 * 1024 * 1024;
        // Each control character takes six JSON bytes, plus two quotes. The
        // remaining two bytes are one UTF-8 character, not two Rust chars.
        let mut text = "\u{1}".repeat((LIMIT - 2) / 6);
        text.push('é');
        assert!(text.len() < LIMIT);
        let mut value = fixture();
        value["fest_attribution_observations"] = Value::String(text.clone());
        // Exactly LIMIT bytes passes the size gate, then fails report shape.
        assert_eq!(check(&value).as_deref(), Some("missing JUnit matrix"));
        text.push('\n');
        value["fest_attribution_observations"] = Value::String(text);
        assert_eq!(
            check(&value).as_deref(),
            Some("Fest attribution observations exceed 4 MiB")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn missing_or_incomplete_cache_evidence_is_never_accepted() {
        let value = json!({"fest_cache":{"schema_version":1,"path":"/private/cache",
            "read_only":true,"initially_empty":true,"removed":true}});
        assert!(cleanup_error(value.as_object().unwrap()).is_none());
        assert!(cleanup_error(&Map::new()).is_some());
        for key in [
            "schema_version",
            "path",
            "read_only",
            "initially_empty",
            "removed",
        ] {
            let mut bad = value.clone();
            bad["fest_cache"].as_object_mut().unwrap().remove(key);
            assert!(cleanup_error(bad.as_object().unwrap()).is_some(), "{key}");
        }
        for key in ["read_only", "initially_empty", "removed"] {
            let mut bad = value.clone();
            bad["fest_cache"][key] = Value::Bool(false);
            assert!(cleanup_error(bad.as_object().unwrap()).is_some(), "{key}");
        }
        for path in ["relative", "/private/\ncache"] {
            let mut bad = value.clone();
            bad["fest_cache"]["path"] = json!(path);
            assert!(cleanup_error(bad.as_object().unwrap()).is_some());
        }
    }
}
