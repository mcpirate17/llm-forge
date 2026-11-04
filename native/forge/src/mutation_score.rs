//! Survivor-set scoring, and the accounting every receipt carries.
//!
//! A generated corpus has no curator, so its fraction killed compares to
//! nothing. Movement does: a mutant that survives today and did not when the
//! baseline was recorded is a test that stopped defending its code.
//!
//! The status here is stricter than the Python runner's. That runner called a
//! run PASS with timed-out, uncovered or unviable rows, none of which the
//! receipt validator accepts as a PASS; here they are INCOMPLETE. A mutant the
//! engine planned and never reported is INCOMPLETE too.

use crate::mutation_campaign::{
    py_repr, record_survivor_baseline, Campaign, ERROR as ROW_ERROR, KILLED, NO_COVERAGE, OUTCOMES,
    SURVIVED, TIMED_OUT, UNVIABLE,
};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const RUNNING: &str = "RUNNING";
pub const PASS: &str = "PASS";
pub const RATCHET_HELD: &str = "RATCHET_HELD";
pub const FAIL: &str = "FAIL";
pub const ERROR: &str = "ERROR";
pub const INCOMPLETE: &str = "INCOMPLETE";
pub const CANCELLED: &str = "CANCELLED";
pub const BASELINE_FAILED: &str = "BASELINE_FAILED";

pub type Receipt = Map<String, Value>;

/// The rows a receipt holds so far.
pub fn rows(receipt: &Receipt) -> &[Value] {
    receipt
        .get("mutants")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

pub fn status(receipt: &Receipt) -> &str {
    receipt
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn outcome_of(row: &Value) -> &str {
    row.get("outcome")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// How many rows carry each outcome; an unknown outcome refuses the run.
pub fn outcome_counts(rows: &[Value]) -> Result<BTreeMap<&'static str, u64>, String> {
    let mut counts: BTreeMap<&'static str, u64> =
        OUTCOMES.iter().map(|outcome| (*outcome, 0)).collect();
    for row in rows {
        let outcome = outcome_of(row);
        let known = OUTCOMES
            .iter()
            .find(|candidate| **candidate == outcome)
            .ok_or_else(|| format!("unknown mutant outcome {}", py_repr(outcome)))?;
        *counts.entry(known).or_insert(0) += 1;
    }
    Ok(counts)
}

/// What one receipt's rows add up to.
pub struct Tally {
    pub counts: BTreeMap<&'static str, u64>,
    /// Planned mutants with no row; None when the engine reported no plan.
    pub unstarted: Option<u64>,
}

/// Record planned, completed, failed, censored and unstarted mutants.
///
/// Every receipt gets this, however the run ended. `planned` is the engine's
/// own count, so a run that stopped early shows the gap instead of hiding it.
pub fn account(receipt: &mut Receipt, planned: Option<u64>) -> Result<Tally, String> {
    let reported = rows(receipt).len() as u64;
    let counts = outcome_counts(rows(receipt))?;
    if let Some(planned) = planned.filter(|planned| reported > *planned) {
        return Err(format!(
            "the engine reported {reported} mutants against a plan of {planned}"
        ));
    }
    let unstarted = planned.map(|planned| planned - reported);
    receipt.insert(
        "accounting".into(),
        json!({
            "planned": planned,
            "completed": counts[KILLED] + counts[SURVIVED],
            "failed": counts[ROW_ERROR] + counts[UNVIABLE],
            "censored": counts[TIMED_OUT] + counts[NO_COVERAGE],
            "unstarted": unstarted,
        }),
    );
    receipt.insert(
        "complete_campaign".into(),
        Value::Bool(unstarted == Some(0) && counts[ROW_ERROR] == 0),
    );
    Ok(Tally { counts, unstarted })
}

/// Whether the engine itself finished: the validator rejects any receipt whose
/// `engine_result` did not, so a clean-looking report must not read as PASS.
fn engine_finished(receipt: &Receipt) -> bool {
    receipt.get("engine_result").is_none_or(|engine| {
        engine.get("returncode").and_then(Value::as_i64) == Some(0)
            && engine.get("timed_out") == Some(&Value::Bool(false))
    })
}

fn verdict(
    tally: &Tally,
    finished: bool,
    reported: usize,
    survivors: usize,
    new_survivors: usize,
) -> &'static str {
    let counts = &tally.counts;
    if counts[ROW_ERROR] > 0 {
        ERROR
    } else if new_survivors > 0 {
        FAIL
    } else if tally.unstarted != Some(0) || reported == 0 {
        INCOMPLETE
    } else if survivors > 0 {
        // Green would be a lie: recorded survivors are known gaps nobody closed.
        RATCHET_HELD
    } else if !finished || counts[TIMED_OUT] + counts[NO_COVERAGE] + counts[UNVIABLE] > 0 {
        INCOMPLETE
    } else {
        PASS
    }
}

/// Score on the survivor set, and record the run's accounting.
pub fn score(
    campaign: &Campaign,
    receipt: &mut Receipt,
    planned: Option<u64>,
) -> Result<(), String> {
    let tally = account(receipt, planned)?;
    let counts = &tally.counts;
    let denominator = counts[KILLED] + counts[SURVIVED];
    let mutation_score = (denominator > 0).then(|| counts[KILLED] as f64 / denominator as f64);
    let survivors: BTreeSet<String> = rows(receipt)
        .iter()
        .filter(|row| outcome_of(row) == SURVIVED)
        .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let baseline: BTreeSet<String> = campaign.survivor_baseline.iter().cloned().collect();
    let new_survivors: Vec<&String> = survivors.difference(&baseline).collect();
    let resolved: Vec<&String> = baseline.difference(&survivors).collect();
    let verdict = verdict(
        &tally,
        engine_finished(receipt),
        rows(receipt).len(),
        survivors.len(),
        new_survivors.len(),
    );
    let outcome_counts: Map<String, Value> = counts
        .iter()
        .map(|(outcome, count)| ((*outcome).to_string(), json!(count)))
        .collect();
    let scored = [
        ("outcome_counts", Value::Object(outcome_counts)),
        ("mutation_score", json!(mutation_score)),
        ("no_coverage", json!(counts[NO_COVERAGE])),
        ("unviable", json!(counts[UNVIABLE])),
        ("timed_out", json!(counts[TIMED_OUT])),
        ("survivors", json!(survivors)),
        ("new_survivors", json!(new_survivors)),
        ("resolved_survivors", json!(resolved)),
        ("classification_required", json!(new_survivors)),
        ("status", json!(verdict)),
    ];
    for (key, value) in scored {
        receipt.insert(key.into(), value);
    }
    Ok(())
}

/// Write a first full run's survivors into its manifest, then re-score.
///
/// A run that errored, was cancelled or left mutants unstarted measured less
/// than the corpus, so recording it would bless gaps nobody actually saw.
pub fn record_baseline(
    campaign: &mut Campaign,
    receipt: &mut Receipt,
    planned: Option<u64>,
) -> Result<bool, String> {
    let settled = !matches!(status(receipt), ERROR | CANCELLED);
    let every_mutant_ran = receipt
        .get("accounting")
        .and_then(|accounting| accounting.get("unstarted"))
        .and_then(Value::as_u64)
        == Some(0);
    if !settled || !every_mutant_ran {
        return Ok(false);
    }
    let survivors: Vec<String> = receipt
        .get("survivors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect();
    if !record_survivor_baseline(campaign, &survivors)? {
        return Ok(false);
    }
    receipt.insert(
        "manifest_sha256".into(),
        json!(campaign.manifest_sha256.clone()),
    );
    receipt.insert(
        "survivor_baseline_recorded_by_this_run".into(),
        Value::Bool(true),
    );
    score(campaign, receipt, planned)?;
    Ok(true)
}

#[cfg(test)]
#[path = "mutation_score_tests.rs"]
mod tests;
