//! Cooperative v3 test-selection observations bound to a verified campaign.
use crate::mutation_workspace_selector::{Selector, Target};
use serde_json::{Map, Value};

fn digest(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn observation(value: &Value, selector: &Selector) -> bool {
    let Some(row) = value.as_object() else {
        return false;
    };
    let passed = row.get("passed").and_then(Value::as_u64);
    let ignored = row.get("ignored").and_then(Value::as_u64);
    let filtered = row.get("filtered").and_then(Value::as_u64);
    row.len() == 6
        && row.get("frames").and_then(Value::as_u64) == Some(1)
        && passed.is_some_and(|n| n > 0)
        && passed
            .zip(ignored)
            .and_then(|(p, i)| p.checked_add(i))
            .is_some_and(|n| n <= 100_000)
        && filtered.is_some_and(|n| n <= 100_000 && (selector.filter.is_some() || n == 0))
        && row.get("inventory_sha256").is_some_and(digest)
        && row.get("scope").and_then(Value::as_str)
            == Some("observed selected libtest output, not producer attestation")
}

fn selected_target(execution: &Value, selector: &Selector) -> Result<(), String> {
    let target = execution
        .get("selected_target")
        .and_then(Value::as_object)
        .ok_or("selected target missing")?;
    if target.len() != 3 {
        return Err("invalid selected target shape".into());
    }
    let name = target
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
        .ok_or("invalid selected target name")?;
    let kind = match &selector.target {
        Target::Lib {} => "lib",
        Target::Test { name: expected } if expected == name => "test",
        Target::Test { .. } => return Err("selected target name differs".into()),
    };
    if target.get("kind").and_then(Value::as_str) != Some(kind) {
        return Err("selected target kind differs".into());
    }
    let source = target
        .get("source")
        .and_then(Value::as_str)
        .ok_or("selected target source missing")?;
    crate::mutation_snapshot_inputs::validate_path(source)?;
    if !execution
        .get("targets")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .filter(|v| {
                    v.as_str().and_then(|s| s.rsplit_once(':')).is_some_and(
                        |(kinds, target_name)| {
                            target_name == name
                                && if kind == "lib" {
                                    let values: Vec<&str> = kinds.split(',').collect();
                                    !values.is_empty()
                                        && values
                                            .iter()
                                            .copied()
                                            .collect::<std::collections::BTreeSet<_>>()
                                            .len()
                                            == values.len()
                                        && values.iter().all(|k| {
                                            crate::mutation_workspace_selector::is_library_kind(k)
                                        })
                                } else {
                                    kinds == "test"
                                }
                        },
                    )
                })
                .count()
                == 1
        })
    {
        return Err("selected target absent from metadata inventory".into());
    }
    Ok(())
}

fn engine_prefix(
    args: &[&str],
    base: &[String],
    tail_len: usize,
    sources: &Value,
) -> Result<(), String> {
    let manifest = if base[3] == "Cargo.toml" {
        "./Cargo.toml"
    } else {
        &base[3]
    };
    if args.len() < 12
        || args[1] != "mutants"
        || args[2] != "--manifest-path"
        || args[3] != manifest
        || args[4] != "--in-place"
        || args[5] != "--timeout"
        || args[6].parse::<u64>().ok().filter(|n| *n > 0).is_none()
        || args[7] != "--output"
        || args[8].is_empty()
        || args[9] != "--package"
        || args[10] != base[5]
    {
        return Err("invalid workspace engine prefix".into());
    }
    let mut index = 11;
    let mut files = 0;
    let pins = sources
        .as_object()
        .filter(|pins| !pins.is_empty())
        .ok_or("verified source pins unavailable")?;
    let mut matched = std::collections::BTreeSet::new();
    let mut prefix: Option<&str> = None;
    while index < args.len() && args[index] == "--file" {
        if index + 1 >= args.len() || args[index + 1].is_empty() || args[index + 1].starts_with('-')
        {
            return Err("invalid workspace engine source filter".into());
        }
        let file = args[index + 1];
        crate::mutation_snapshot_inputs::validate_path(file)?;
        if !file.contains('/') || file.contains(['*', '?', '[', ']', '{', '}', '\\']) {
            return Err("engine source must be literal".into());
        }
        if prefix.is_none() {
            let candidates: Vec<&str> = pins
                .keys()
                .filter_map(|pin| {
                    if pin == file {
                        Some("")
                    } else {
                        pin.strip_suffix(file).filter(|head| head.ends_with('/'))
                    }
                })
                .take(2)
                .collect();
            if candidates.len() != 1 {
                return Err("engine source does not uniquely match verified pins".into());
            }
            prefix = Some(candidates[0]);
        }
        let pin = format!("{}{file}", prefix.unwrap_or_default());
        if !pins.contains_key(&pin) || !matched.insert(pin) {
            return Err("engine source does not uniquely match verified pins".into());
        }
        files += 1;
        index += 2;
    }
    if files == 0
        || matched.len() != pins.len()
        || args.len() != index + 3 + tail_len
        || args.get(index..index + 3)
            != Some(&[
                "--no-config",
                "--cargo-arg=--locked",
                "--cargo-arg=--offline",
            ])
    {
        return Err("unexpected workspace engine option".into());
    }
    Ok(())
}

fn argv_checks(
    payload: &Map<String, Value>,
    selector: &Selector,
    expected: &[String],
    sources: &Value,
) -> Result<(), String> {
    let mut suffix = selector.cargo_args();
    suffix.extend(selector.libtest_args());
    if !expected.ends_with(&suffix) || expected.len() < suffix.len() + 6 {
        return Err("invalid verified test argv".into());
    }
    let base = &expected[..expected.len() - suffix.len()];
    if base.len() != 6
        || base[0] != "cargo"
        || base[1] != "test"
        || base[2] != "--manifest-path"
        || base[4] != "--package"
    {
        return Err("invalid verified test argv".into());
    }
    let mut baseline = base.to_vec();
    baseline.extend(["--locked".into(), "--offline".into()]);
    baseline.extend(suffix);
    if payload.get("test_argv") != Some(&serde_json::json!(expected))
        || payload.get("baseline_argv") != Some(&serde_json::json!(baseline))
    {
        return Err("selected test argv differs from campaign".into());
    }
    let rows = payload
        .get("process_commands")
        .and_then(Value::as_array)
        .filter(|v| v.len() == 3)
        .ok_or("selected command ledger must have three commands")?;
    let metadata = vec![
        "cargo",
        "metadata",
        "--manifest-path",
        &base[3],
        "--format-version=1",
        "--locked",
        "--offline",
    ];
    if rows[0].get("argv") != Some(&serde_json::json!(metadata))
        || rows[1].get("argv") != Some(&serde_json::json!(baseline))
    {
        return Err("selected metadata or baseline argv differs".into());
    }
    let args = rows[2]
        .get("argv")
        .and_then(Value::as_array)
        .ok_or("selected engine argv missing")?;
    let args: Vec<&str> = args
        .iter()
        .map(|v| v.as_str().ok_or("invalid engine argv"))
        .collect::<Result<_, _>>()?;
    let mut tail = vec!["--test-package".to_owned(), base[5].clone()];
    tail.extend(selector.engine_args());
    engine_prefix(&args, base, tail.len(), sources)?;
    if args.len() < 2
        || args[1] != "mutants"
        || !args.ends_with(&tail.iter().map(String::as_str).collect::<Vec<_>>())
    {
        return Err("selected engine argv differs".into());
    }
    let forwarded: Vec<&str> = args
        .iter()
        .copied()
        .filter(|s| s.starts_with("--cargo-arg=") || s.starts_with("--cargo-test-arg="))
        .collect();
    let mut permitted = vec![
        "--cargo-arg=--locked".to_owned(),
        "--cargo-arg=--offline".to_owned(),
    ];
    permitted.extend(selector.engine_args());
    if forwarded != permitted.iter().map(String::as_str).collect::<Vec<_>>()
        || args.iter().filter(|s| **s == "--test-package").count() != 1
        || args.iter().any(|s| {
            *s == "--cargo-arg" || *s == "--cargo-test-arg" || s.starts_with("--test-package=")
        })
    {
        return Err("unexpected engine test forwarding".into());
    }
    Ok(())
}

/// Validate receipt claims against an independently verified campaign selector.
pub fn validate(
    profile: Option<&str>,
    selector: Option<&Selector>,
    expected_test_argv: &[String],
    expected_sources: &Value,
    payload: &Map<String, Value>,
) -> Result<(), String> {
    let execution = payload.get("workspace_execution").unwrap_or(&Value::Null);
    if profile != Some("cargo-workspace-v3") {
        if selector.is_some()
            || execution.get("profile").and_then(Value::as_str) == Some("cargo-workspace-v3")
            || execution.get("selector").is_some()
        {
            return Err("workspace v3 receipt/profile mismatch".into());
        }
        return Ok(());
    }
    let selector = selector.ok_or("verified workspace selector unavailable")?;
    selector.validate()?;
    if execution.get("profile").and_then(Value::as_str) != profile
        || execution.get("selector")
            != Some(&serde_json::to_value(selector).map_err(|e| e.to_string())?)
        || execution.get("test_scope").and_then(Value::as_str)
            != Some("selected-target-full-source-snapshot")
    {
        return Err("workspace selection differs from verified campaign".into());
    }
    selected_target(execution, selector)?;
    if expected_test_argv.get(5).map(String::as_str)
        != execution.get("package").and_then(Value::as_str)
    {
        return Err("selected package differs from verified campaign".into());
    }
    let baseline = payload
        .get("workspace_baseline")
        .ok_or("selected baseline missing")?;
    let direct = baseline
        .get("direct")
        .ok_or("direct selected baseline missing")?;
    if !observation(direct, selector)
        || baseline.get("engine") != Some(direct)
        || !baseline.get("engine_log_sha256").is_some_and(digest)
    {
        return Err("selected baseline observations differ or are incomplete".into());
    }
    argv_checks(payload, selector, expected_test_argv, expected_sources)
}

#[cfg(test)]
#[path = "mutation_workspace_receipt_tests.rs"]
pub(crate) mod tests;
