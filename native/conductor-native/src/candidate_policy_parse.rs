//! Candidate policy schema validation. Input is JSON from Python's TOML decoder.

use super::candidate_policy_value::*;
use super::candidate_policy_waivers::{mutation_waivers, parse_value_waivers};
use serde_json::{json, Value};
use std::collections::HashSet;

const TOP: &[&str] = &[
    "schema_version",
    "block_at",
    "max_workers",
    "cache_ttl_days",
    "claim_max_age_hours",
    "max_file_bytes",
    "max_binary_bytes",
    "coverage_threshold",
    "high_risk_coverage_threshold",
    "baseline_expires",
    "classes",
    "risk",
    "paths",
    "checks",
    "baselines",
    "exceptions",
    "mutation_waivers",
    "value_waivers",
    "tools",
];
const CLASSES: &[&str] = &[
    "binary",
    "cfamily",
    "cfamily_host",
    "config",
    "cuda",
    "dependency",
    "docs",
    "generated",
    "governance",
    "native",
    "notebook",
    "novel",
    "node_dependency",
    "python",
    "python_dependency",
    "research_result",
    "rust",
    "rust_dependency",
    "shell",
    "source",
    "symlink",
    "test",
    "toml",
    "workflow",
    "web",
];
const CHECK_KEYS: &[&str] = &[
    "kind",
    "profiles",
    "classes",
    "exclude_classes",
    "command",
    "version_command",
    "severity",
    "timeout_seconds",
    "wall_timeout_seconds",
    "memory_mb",
    "always",
    "cache",
    "run_on_deletions",
    "max_output_chars",
    "attribution",
    "shard_max_files",
    "shard_workers",
];
const EXCEPTION_KEYS: &[&str] = &[
    "id",
    "check",
    "rule",
    "path",
    "fingerprint",
    "owner",
    "justification",
    "expires",
];

fn class_list(value: &Value, field: &str) -> Result<Vec<String>> {
    let list = strings(value, field, true)?;
    if list.iter().any(|s| !CLASSES.contains(&s.as_str())) {
        Err(format!("{field} contains an unknown class"))
    } else {
        Ok(list)
    }
}

fn check(id: &str, entry: &Value) -> Result<Value> {
    let field = format!("checks.{id}");
    let raw = table(entry, &format!("{field} must be a table"))?;
    unknown(raw, CHECK_KEYS, &format!("{field} has unknown keys"))?;
    let kind = value(raw, "kind").as_str().unwrap_or("");
    if !["builtin", "command"].contains(&kind) {
        return Err(format!("{field}.kind must be builtin or command"));
    }
    let profiles = strings(value(raw, "profiles"), &format!("{field}.profiles"), false)?;
    if profiles.iter().any(|s| s != "fast" && s != "full") {
        return Err(format!("{field}.profiles contains an unknown profile"));
    }
    let classes = class_list(
        &default(raw, "classes", json!([])),
        &format!("{field}.classes"),
    )?;
    let exclude_classes = class_list(
        &default(raw, "exclude_classes", json!([])),
        &format!("{field}.exclude_classes"),
    )?;
    let command = strings(
        &default(raw, "command", json!([])),
        &format!("{field}.command"),
        true,
    )?;
    let version_command = strings(
        &default(raw, "version_command", json!([])),
        &format!("{field}.version_command"),
        true,
    )?;
    if kind == "command" && (command.is_empty() || version_command.is_empty()) {
        return Err(format!(
            "command check {id} requires command and version_command"
        ));
    }
    let sev = severity(
        &default(raw, "severity", json!("high")),
        &format!("{field}.severity is invalid"),
    )?;
    let attribution = default(raw, "attribution", json!("candidate"));
    if !matches!(attribution.as_str(), Some("candidate" | "diff")) {
        return Err(format!(
            "{field}.attribution must be one of ['candidate', 'diff']"
        ));
    }
    Ok(
        json!({"check_id": id, "kind": kind, "profiles": profiles, "classes": classes,
        "exclude_classes": exclude_classes, "command": command, "version_command": version_command,
        "severity": sev, "timeout_seconds": positive(&default(raw,"timeout_seconds",json!(60)), &format!("{field}.timeout_seconds"), Some(3600))?,
        "memory_mb": positive(&default(raw,"memory_mb",json!(2048)), &format!("{field}.memory_mb"), Some(65536))?,
        "attribution": attribution, "always": boolean(&default(raw,"always",json!(false)), &format!("{field}.always"))?,
        "cache": boolean(&default(raw,"cache",json!(true)), &format!("{field}.cache"))?,
        "run_on_deletions": boolean(&default(raw,"run_on_deletions",json!(false)), &format!("{field}.run_on_deletions"))?,
        "max_output_chars": positive(&default(raw,"max_output_chars",json!(12000)), &format!("{field}.max_output_chars"), None)?,
        "shard_max_files": nonnegative(&default(raw,"shard_max_files",json!(0)), &format!("{field}.shard_max_files"), None)?,
        "shard_workers": positive(&default(raw,"shard_workers",json!(1)), &format!("{field}.shard_workers"), Some(32))?,
        "wall_timeout_override": nonnegative(&default(raw,"wall_timeout_seconds",json!(0)), &format!("{field}.wall_timeout_seconds"), None)?}),
    )
}

fn exception_path(path: &str) -> Result<()> {
    if ["*", "**", "**/*", ".", "./*"].contains(&path) || path.starts_with('/') {
        return Err(format!(
            "exception path is a forbidden blanket scope: '{path}'"
        ));
    }
    let literal = path
        .split('/')
        .filter(|part| !part.chars().any(|c| "*?[".contains(c)) && !part.is_empty() && *part != ".")
        .count();
    if literal < 2 {
        return Err(format!(
            "exception path must have at least two literal segments: '{path}'"
        ));
    }
    Ok(())
}

fn exception(entry: &Value) -> Result<Value> {
    let raw = table(entry, "each exceptions entry must be a table")?;
    unknown(raw, EXCEPTION_KEYS, "exception has unknown keys")?;
    missing(
        raw,
        &["id", "check", "path", "owner", "justification", "expires"],
        "exception is missing required keys",
    )?;
    let path = py_string(value(raw, "path"));
    exception_path(&path)?;
    let owner = py_string(value(raw, "owner")).trim().to_owned();
    let justification = py_string(value(raw, "justification")).trim().to_owned();
    if owner.len() < 2 || justification.len() < 20 {
        return Err("exception owner/justification is not specific enough".into());
    }
    let optional = |key| {
        if value(raw, key).is_null() || value(raw, key) == &json!("") {
            None
        } else {
            Some(py_string(value(raw, key)))
        }
    };
    Ok(
        json!({"exception_id": py_string(value(raw,"id")), "check_id": py_string(value(raw,"check")),
        "rule_id": optional("rule"), "path": path, "fingerprint": optional("fingerprint"),
        "owner": owner, "justification": justification, "expires": date(value(raw,"expires"), "exceptions.expires")?}),
    )
}

fn tools(raw: &Value) -> Result<Vec<Value>> {
    if raw.is_null() {
        return Ok(vec![]);
    }
    let map = table(raw, "tools must be a table")?;
    let keys = [
        "executable",
        "version_command",
        "expected_version",
        "required_profiles",
        "provided_by",
        "rationale",
    ];
    let mut out = Vec::with_capacity(map.len());
    for (id, entry) in map {
        let Some(tool) = entry.as_object().filter(|t| exact_keys(t, &keys)) else {
            return Err(format!(
                "tools.{id} has an invalid schema; required keys: {}",
                repr_list(&keys.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
            ));
        };
        out.push(json!({"tool_id": id, "executable": py_string(value(tool,"executable")),
            "version_command": strings(value(tool,"version_command"), &format!("tools.{id}.version_command"), true)?,
            "expected_version": py_string(value(tool,"expected_version")),
            "required_profiles": strings(value(tool,"required_profiles"), &format!("tools.{id}.required_profiles"), true)?,
            "provided_by": py_string(value(tool,"provided_by")), "rationale": py_string(value(tool,"rationale"))}));
    }
    Ok(out)
}

fn baselines(raw: &Value) -> Result<Vec<Value>> {
    let map = table(raw, "baselines must be a table")?;
    let mut out = Vec::with_capacity(map.len());
    for (id, entry) in map {
        let Some(base) = entry
            .as_object()
            .filter(|b| exact_keys(b, &["path", "classes", "required_profiles"]))
        else {
            return Err(format!("baselines.{id} has an invalid schema"));
        };
        out.push(json!({"baseline_id": id, "path": py_string(value(base,"path")),
            "classes": strings(value(base,"classes"), &format!("baselines.{id}.classes"), true)?,
            "required_profiles": strings(value(base,"required_profiles"), &format!("baselines.{id}.required_profiles"), true)?}));
    }
    Ok(out)
}

fn days_from_civil(date: &str) -> i64 {
    let mut parts = date.split('-').map(|part| part.parse::<i64>().unwrap_or(0));
    let mut y = parts.next().unwrap_or(0);
    let m = parts.next().unwrap_or(0);
    let d = parts.next().unwrap_or(0);
    y -= i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn validate_dates(policy: &Value, today: &str) -> Result<()> {
    let now = days_from_civil(today);
    let expires = policy["baseline_expires"].as_str().unwrap_or("");
    if days_from_civil(expires) < now {
        return Err(format!(
            "policy baseline window expired on {expires}; refresh and re-review it"
        ));
    }
    let exceptions = policy["exceptions"].as_array().expect("parsed exceptions");
    let mut seen = HashSet::new();
    for item in exceptions {
        let id = item["exception_id"].as_str().unwrap_or("");
        if !seen.insert(id) {
            return Err("exception identifiers must be unique".into());
        }
    }
    let checks: HashSet<_> = policy["checks"]
        .as_array()
        .expect("parsed checks")
        .iter()
        .filter_map(|v| v["check_id"].as_str())
        .collect();
    for item in exceptions {
        let id = item["exception_id"].as_str().unwrap_or("");
        if !checks.contains(item["check_id"].as_str().unwrap_or("")) {
            return Err(format!("exception {id} names an unknown check"));
        }
        let expiry = item["expires"].as_str().unwrap_or("");
        let days = days_from_civil(expiry) - now;
        if days < 0 {
            return Err(format!("exception {id} expired on {expiry}"));
        }
        if days > 90 {
            return Err(format!("exception {id} expires more than 90 days out"));
        }
    }
    for item in policy["mutation_waivers"]
        .as_array()
        .expect("parsed waivers")
    {
        let id = item["waiver_id"].as_str().unwrap_or("");
        let expiry = item["expires"].as_str().unwrap_or("");
        let days = days_from_civil(expiry) - now;
        if days < 0 {
            return Err(format!("mutation waiver {id} expired on {expiry}"));
        }
        if days > 90 {
            return Err(format!(
                "mutation waiver {id} expires more than 90 days out"
            ));
        }
    }
    Ok(())
}

pub fn parse_policy(raw: &Value, today: &str) -> Result<Value> {
    let map = table(raw, "candidate policy must be a table")?;
    unknown(map, TOP, "candidate policy has unknown top-level keys")?;
    let schema = value(map, "schema_version");
    if schema.as_f64() != Some(1.0) && schema.as_bool() != Some(true) {
        return Err(format!(
            "unsupported policy schema_version: {}",
            repr(schema)
        ));
    }
    let class_raw = table(value(map, "classes"), "classes must be a table")?;
    unknown(class_raw, CLASSES, "classes contains unknown names")?;
    let mut classes = Object::new();
    for (name, patterns) in class_raw {
        insert(
            &mut classes,
            name,
            strings(patterns, &format!("classes.{name}"), true)?,
        );
    }
    let risk = table(
        value(map, "risk"),
        "risk must contain exactly the high array",
    )?;
    if !exact_keys(risk, &["high"]) {
        return Err("risk must contain exactly the high array".into());
    }
    let paths = table(
        value(map, "paths"),
        "paths must contain exactly protected_deletes, hot, and generated",
    )?;
    if !exact_keys(paths, &["protected_deletes", "hot", "generated"]) {
        return Err("paths must contain exactly protected_deletes, hot, and generated".into());
    }
    let checks = table(value(map, "checks"), "checks must be a non-empty table")?;
    if checks.is_empty() {
        return Err("checks must be a non-empty table".into());
    }
    let exceptions = default(map, "exceptions", json!([]));
    let exceptions = exceptions
        .as_array()
        .ok_or("exceptions must be an array of tables")?;
    let block_at = severity(value(map, "block_at"), "block_at must be a valid severity")?;
    let parsed = json!({"schema_version": 1, "block_at": block_at,
        "max_workers": positive(value(map,"max_workers"), "max_workers", Some(16))?,
        "cache_ttl_days": positive(value(map,"cache_ttl_days"), "cache_ttl_days", Some(365))?,
        "claim_max_age_hours": positive(value(map,"claim_max_age_hours"), "claim_max_age_hours", Some(720))?,
        "max_file_bytes": positive(value(map,"max_file_bytes"), "max_file_bytes", None)?,
        "max_binary_bytes": positive(value(map,"max_binary_bytes"), "max_binary_bytes", None)?,
        "coverage_threshold": percent(value(map,"coverage_threshold"), "coverage_threshold")?,
        "high_risk_coverage_threshold": percent(value(map,"high_risk_coverage_threshold"), "high_risk_coverage_threshold")?,
        "baseline_expires": date(value(map,"baseline_expires"), "baseline_expires")?,
        "class_globs": classes, "high_risk_globs": strings(value(risk,"high"), "risk.high", true)?,
        "protected_delete_globs": strings(value(paths,"protected_deletes"), "paths.protected_deletes", true)?,
        "hot_path_globs": strings(value(paths,"hot"), "paths.hot", true)?,
        "generated_globs": strings(value(paths,"generated"), "paths.generated", true)?,
        "checks": checks.iter().map(|(id, entry)| check(id,entry)).collect::<Result<Vec<_>>>()?,
        "baselines": baselines(&default(map,"baselines",json!({})))?,
        "exceptions": exceptions.iter().map(exception).collect::<Result<Vec<_>>>()?,
        "mutation_waivers": mutation_waivers(value(map,"mutation_waivers"))?,
        "value_waivers": parse_value_waivers(value(map,"value_waivers"))?,
        "tools": tools(value(map,"tools"))?});
    validate_dates(&parsed, today)?;
    Ok(parsed)
}

pub fn fragment(operation: &str, raw: &Value, today: &str) -> Result<Value> {
    match operation {
        "check" => check(raw["id"].as_str().unwrap_or(""), &raw["value"]),
        "exception" => exception(raw),
        "exception_path" => {
            exception_path(raw.as_str().unwrap_or(""))?;
            Ok(Value::Null)
        }
        "baselines" => Ok(json!(baselines(raw)?)),
        "validate" => {
            validate_dates(raw, today)?;
            Ok(Value::Null)
        }
        "attribution" => {
            let field = raw["field"].as_str().unwrap_or("attribution");
            match raw["value"].as_str() {
                Some("candidate" | "diff") => Ok(raw["value"].clone()),
                _ => Err(format!("{field} must be one of ['candidate', 'diff']")),
            }
        }
        "strings" => Ok(json!(strings(
            &raw["value"],
            raw["field"].as_str().unwrap_or("value"),
            raw["allow_empty"].as_bool().unwrap_or(true)
        )?)),
        "positive" => Ok(json!(positive(
            &raw["value"],
            raw["field"].as_str().unwrap_or("value"),
            raw["maximum"].as_u64()
        )?)),
        "nonnegative" => Ok(json!(nonnegative(
            &raw["value"],
            raw["field"].as_str().unwrap_or("value"),
            raw["maximum"].as_u64()
        )?)),
        "percent" => Ok(json!(percent(
            &raw["value"],
            raw["field"].as_str().unwrap_or("value")
        )?)),
        "boolean" => Ok(json!(boolean(
            &raw["value"],
            raw["field"].as_str().unwrap_or("value")
        )?)),
        "date" => Ok(json!(date(
            &raw["value"],
            raw["field"].as_str().unwrap_or("value")
        )?)),
        _ => Err(format!("unknown candidate policy fragment: {operation}")),
    }
}
