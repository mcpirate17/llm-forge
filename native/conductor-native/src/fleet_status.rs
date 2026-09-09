//! Deterministic aggregation and presentation for the fleet-status CLI.
//!
//! Python owns the intentionally small process and filesystem boundary.  This
//! module owns the data-only join, ordering, and human-readable rendering so
//! operational state is represented with Rust collections rather than loose
//! Python loops.

use std::collections::{BTreeMap, BTreeSet};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde_json::{json, Map, Value};

fn parse_object(raw: &str, label: &str) -> PyResult<Map<String, Value>> {
    serde_json::from_str::<Value>(raw)
        .map_err(|error| {
            PyValueError::new_err(format!("fleet status {label} is invalid JSON: {error}"))
        })?
        .as_object()
        .cloned()
        .ok_or_else(|| PyValueError::new_err(format!("fleet status {label} must be a JSON object")))
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value
        .as_object()
        .and_then(|object| object.get(field))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn display_value(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| value.map(Value::to_string))
        .unwrap_or_default()
}

fn prefix(value: &str, count: usize) -> String {
    value.chars().take(count).collect()
}

fn heading_seat(heading: &str) -> String {
    heading
        .rsplit_once(',')
        .map(|(_, seat)| seat.trim())
        .filter(|seat| {
            !seat.is_empty()
                && seat.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-')
                })
        })
        .unwrap_or_default()
        .to_owned()
}

#[pyfunction]
fn fleet_status_build_report_native(
    peers_json: &str,
    heard_json: &str,
    state_json: &str,
    worktree_processes_json: &str,
    generated_at: &str,
    root: &str,
) -> PyResult<String> {
    let peers = parse_object(peers_json, "peers")?;
    let heard = parse_object(heard_json, "heard")?;
    let state = parse_object(state_json, "state")?;
    let worktree_processes = parse_object(worktree_processes_json, "worktree processes")?;
    let claims = state
        .get("active_claims")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let headings = state
        .get("active_headings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut names = BTreeSet::new();
    names.extend(peers.keys().cloned());
    names.extend(heard.keys().cloned());
    for claim in &claims {
        if let Some(owner) = string_field(claim, "owner") {
            if !owner.is_empty() {
                names.insert(owner);
            }
        }
    }
    for heading in &headings {
        if let Some(heading) = heading.as_str() {
            let seat = heading_seat(heading);
            if !seat.is_empty() {
                names.insert(seat);
            }
        }
    }

    let mut seats = BTreeMap::new();
    for name in names {
        let peer = peers.get(&name);
        let a2a = match peer.and_then(|value| value.as_object()) {
            Some(peer) if peer.get("status").and_then(Value::as_str) == Some("up") => {
                format!("up:{}", display_value(peer.get("port")))
            }
            Some(_) => "down".to_owned(),
            None => "NO IDENTITY".to_owned(),
        };
        let seat_claims = claims
            .iter()
            .filter(|claim| string_field(claim, "owner").as_deref() == Some(name.as_str()))
            .collect::<Vec<_>>();
        let mut claim_paths = BTreeSet::new();
        let mut expiries = Vec::new();
        for claim in &seat_claims {
            if let Some(paths) = claim
                .as_object()
                .and_then(|object| object.get("paths"))
                .and_then(Value::as_array)
            {
                claim_paths.extend(
                    paths
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned),
                );
            }
            if let Some(expiry) = string_field(claim, "expires_at") {
                if !expiry.is_empty() {
                    expiries.push(expiry);
                }
            }
        }
        expiries.sort();
        let seat_headings = headings
            .iter()
            .filter_map(Value::as_str)
            .filter(|heading| heading_seat(heading) == name)
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        seats.insert(
            name.clone(),
            json!({
                "a2a": a2a,
                "last_heard": heard.get(&name),
                "headings": seat_headings,
                "claims": seat_claims.len(),
                "claim_paths": claim_paths.into_iter().collect::<Vec<_>>(),
                "soonest_expiry": expiries.first(),
            }),
        );
    }
    serde_json::to_string(&json!({
        "generated_at": generated_at,
        "root": root,
        "seats": seats,
        "worktree_processes": worktree_processes,
    }))
    .map_err(|error| PyValueError::new_err(format!("fleet status serialization failed: {error}")))
}

#[pyfunction]
fn fleet_status_render_native(report_json: &str) -> PyResult<String> {
    let report = parse_object(report_json, "report")?;
    let generated_at = report
        .get("generated_at")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let root = report
        .get("root")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut lines = vec![format!("FLEET STATUS  {generated_at}  root={root}")];
    let seats = report
        .get("seats")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (name, seat) in seats {
        let seat = seat.as_object().ok_or_else(|| {
            PyValueError::new_err(format!("fleet status seat {name:?} must be an object"))
        })?;
        let expiry = seat
            .get("soonest_expiry")
            .and_then(Value::as_str)
            .map(|value| format!("  soonest-expiry={}Z", prefix(value, 16)))
            .unwrap_or_default();
        let a2a = seat.get("a2a").and_then(Value::as_str).unwrap_or_default();
        let claims = seat.get("claims").map(Value::to_string).unwrap_or_default();
        lines.push(format!("\n● {name}  [{a2a}]  claims={claims}{expiry}"));
        for heading in seat
            .get("headings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            lines.push(format!("    heading: {heading}"));
        }
        if let Some(last_heard) = seat.get("last_heard").and_then(Value::as_object) {
            let at = last_heard
                .get("at")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let said = last_heard
                .get("said")
                .and_then(Value::as_str)
                .unwrap_or_default();
            lines.push(format!("    last heard {}: {said}", prefix(at, 19)));
        }
        let paths = seat
            .get("claim_paths")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !paths.is_empty() {
            let shown = paths
                .iter()
                .take(4)
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            let extra = paths.len().saturating_sub(shown.len());
            lines.push(format!(
                "    owns: {}{}",
                shown.join(", "),
                if extra == 0 {
                    String::new()
                } else {
                    format!(" (+{extra} more)")
                }
            ));
        }
    }
    let worktree_processes = report
        .get("worktree_processes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if !worktree_processes.is_empty() {
        lines.push("\nLIVE PROCESSES BY WORKTREE".to_owned());
        for (tree, processes) in worktree_processes {
            let processes = processes.as_array().cloned().unwrap_or_default();
            lines.push(format!("  {tree}: {}", processes.len()));
            if !tree.starts_with("/tmp/") {
                continue;
            }
            for process in processes.iter().take(3).filter_map(Value::as_str) {
                lines.push(format!("    {process}"));
            }
            if processes.len() > 3 {
                lines.push(format!("    … +{} more", processes.len() - 3));
            }
        }
    }
    Ok(lines.join("\n"))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(fleet_status_build_report_native, module)?)?;
    module.add_function(wrap_pyfunction!(fleet_status_render_native, module)?)?;
    Ok(())
}
