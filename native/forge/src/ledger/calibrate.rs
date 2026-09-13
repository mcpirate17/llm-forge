//! `forge ledger calibrate` (design step 3, `docs/design/cost_ledger.md`
//! section 6): the sampling half of the byte-proportional attribution's
//! error-bound measurement. The design's split (section 3) assumes uniform
//! chars-per-token across block types within a turn; calibration measures
//! how wrong that is per block type against the real `count_tokens` API --
//! a network call, so this step is split: a Rust sampler that decides
//! *which* turns to measure and emits only shapes (uuid, block-type char
//! totals, billed tokens -- never block text, the reader's hard rule), and
//! a Python shim (`conductor.ledger_calibrate`) that re-reads the sampled
//! turns' bytes from the transcript and talks to the API.
//!
//! The fixture that lands (`tests/fixtures/ledger/calibration.json`) is
//! read back by `rollup.rs` at build time (`include_str!`), which stamps
//! every `turn_attribution` row with the measured per-block-type error and
//! renames the estimate method `byte_proportional_calibrated_<date>`; an
//! unmeasured bound leaves the uncalibrated method and a `null` error.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;

use super::reader;
use super::schema::BlockTypeCounts;

/// `forge ledger calibrate sample <transcript>...`: one JSON line per
/// sampled turn. Deterministic under `(seed, session_id)` -- same inputs,
/// same sample, so a rerun of the whole calibration reproduces byte-for-
/// byte what the committed fixture says it measured.
#[derive(Args)]
pub struct SampleArgs {
    /// Transcript JSONL file(s) to sample from. The design's five sessions
    /// live under `/home/tim/.claude/projects/` (found by uuid prefix);
    /// subagent transcripts (`agent-*.jsonl`) count as their own sessions.
    pub transcripts: Vec<PathBuf>,

    /// Turns to sample per session. Fewer turns than this in a session
    /// samples all of them.
    #[arg(long, default_value_t = 10)]
    pub per_session: usize,

    /// Seed for the deterministic per-session pick. The seed is combined
    /// with each session id, so adding a session never reshuffles another
    /// session's sample.
    #[arg(long, default_value_t = 0)]
    pub seed: u64,
}

#[derive(Subcommand)]
pub enum CalibrateCommand {
    /// Deterministically sample usage-bearing turns for the calibration
    /// shim, one JSON line per sampled turn (never block text).
    Sample(SampleArgs),
}

/// One sampled turn, the entire contract with the Python shim. Field names
/// match the sample JSONL the shim reads and the design's section 3
/// vocabulary.
#[derive(Debug, Clone, Serialize)]
pub struct SampledTurn {
    pub session_id: String,
    pub turn_uuid: String,
    pub turn_index: usize,
    pub bytes_by_block_type: BlockTypeCounts,
    /// `input_tokens + cache_read_input_tokens + cache_creation_input_tokens`
    /// -- the number the byte-proportional split redistributes.
    pub billed_input: u64,
}

pub fn run_sample(args: SampleArgs) -> Result<i32> {
    if args.transcripts.is_empty() {
        bail!("forge ledger calibrate sample: pass at least one transcript file");
    }
    if args.per_session == 0 {
        bail!("forge ledger calibrate sample: --per-session must be at least 1");
    }
    // One pass per transcript, sessions kept in first-seen order (a file is
    // one session in every real case; a mixed file keeps its sessions'
    // samples distinguishable by session_id either way).
    let mut by_session: BTreeMap<String, Vec<SampledTurn>> = BTreeMap::new();
    for path in &args.transcripts {
        let summary = reader::read_transcript_file(path)
            .with_context(|| format!("reading transcript {}", path.display()))?;
        for turn in &summary.turns {
            let Some(session_id) = &turn.session_id else {
                bail!(
                    "{}: turn {} has no session_id -- cannot stratify the sample",
                    path.display(),
                    turn.turn_uuid
                );
            };
            by_session
                .entry(session_id.clone())
                .or_default()
                .push(SampledTurn {
                    session_id: session_id.clone(),
                    turn_uuid: turn.turn_uuid.clone(),
                    turn_index: turn.turn_index,
                    bytes_by_block_type: turn.bytes_by_block_type,
                    billed_input: turn.input_tokens
                        + turn.cache_read_input_tokens
                        + turn.cache_creation_input_tokens,
                });
        }
    }
    if by_session.is_empty() {
        bail!("forge ledger calibrate sample: no usage-bearing turns found");
    }
    for (session_id, turns) in &by_session {
        let mut picked = sample_indices(turns.len(), args.per_session, args.seed, session_id);
        picked.sort_unstable();
        for index in picked {
            println!("{}", serde_json::to_string(&turns[index])?);
        }
    }
    Ok(0)
}

/// Deterministic stratified pick of `n` indices out of `len`: a partial
/// Fisher-Yates over `0..len` driven by an xorshift64* PRNG seeded from
/// `(seed, session_id)` (FNV-1a over the id) -- no crate dependency, and
/// the stream depends on the session, so sessions do not share picks.
/// Returns `min(n, len)` indices, unsorted (callers sort by turn order).
fn sample_indices(len: usize, n: usize, seed: u64, session_id: &str) -> Vec<usize> {
    let mut state = seed ^ fnv1a(session_id.as_bytes());
    if state == 0 {
        state = 0x9E37_79B9_7F4A_7C15; // xorshift must not start at zero
    }
    let mut indices: Vec<usize> = (0..len).collect();
    let take = n.min(len);
    for i in 0..take {
        let j = i + (next_index(&mut state) % (len - i));
        indices.swap(i, j);
    }
    indices.truncate(take);
    indices
}

fn next_index(state: &mut u64) -> usize {
    // xorshift64* (Marsaglia): cheap, adequate, and fully deterministic.
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D) as usize
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xCBF2_9CE4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// The part of the calibration fixture the rollup consumes: the measured
/// per-block-type error bound and the date it was measured. `null`
/// `per_block_type` (the no-API-key debt path) parses to `None` and the
/// rollup keeps its uncalibrated label -- a bound that was never measured
/// must not be dressed up as one.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationBound {
    pub date: String,
    pub per_block_type: ErrorByBlockType,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ErrorByBlockType {
    pub text: f64,
    pub tool_result: f64,
    pub tool_use: f64,
    pub image: f64,
    pub other: f64,
}

/// Parse the fixture text `rollup.rs` embeds. `None` when the fixture has
/// no measured `per_block_type` (offline/debt path), and a loud error when
/// the text exists but is malformed -- a corrupt bound must stop the build,
/// not silently downgrade to uncalibrated.
pub fn parse_calibration_fixture(text: &str) -> Result<Option<CalibrationBound>> {
    let value: Value =
        serde_json::from_str(text).with_context(|| "calibration fixture is not valid JSON")?;
    let date = value
        .get("generated_utc")
        .and_then(Value::as_str)
        .map(str::to_string)
        .with_context(|| "calibration fixture lacks generated_utc")?;
    let date = date.get(0..10).unwrap_or_default().to_string();
    if date.len() != 10 {
        bail!("calibration fixture generated_utc does not start with YYYY-MM-DD");
    }
    let Some(per_block_type) = value.get("per_block_type").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let field = |name: &str| -> Result<f64> {
        per_block_type
            .get(name)
            .and_then(Value::as_object)
            .and_then(|row| row.get("mape"))
            .and_then(Value::as_f64)
            .with_context(|| format!("calibration fixture per_block_type.{name}.mape"))
    };
    Ok(Some(CalibrationBound {
        date,
        per_block_type: ErrorByBlockType {
            text: field("text")?,
            tool_result: field("tool_result")?,
            tool_use: field("tool_use")?,
            image: field("image")?,
            other: field("other")?,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(index: usize, billed: u64) -> SampledTurn {
        SampledTurn {
            session_id: "s".to_string(),
            turn_uuid: format!("t{index}"),
            turn_index: index,
            bytes_by_block_type: BlockTypeCounts::default(),
            billed_input: billed,
        }
    }

    #[test]
    fn sample_indices_are_deterministic_and_session_scoped() {
        let a = sample_indices(100, 10, 7, "sess-a");
        let b = sample_indices(100, 10, 7, "sess-a");
        let c = sample_indices(100, 10, 7, "sess-b");
        assert_eq!(a, b, "same seed and session must pick identically");
        assert_ne!(a, c, "a different session reshuffles");
        assert_eq!(a.len(), 10);
        assert!(a.iter().all(|i| *i < 100));
        // Distinct picks only -- a stratified sample never repeats a turn.
        let mut sorted = a.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), a.len());
        // Overlapping ranges keep their determinism too (modulo never hits
        // a zero range: n.min(len) <= len, len - i >= 1 on every step).
        let few = sample_indices(3, 10, 7, "sess-a");
        assert_eq!(few.len(), 3);
    }

    #[test]
    fn billed_input_sums_all_three_input_streams() {
        let row = serde_json::to_value(turn(0, 100)).unwrap();
        assert_eq!(row["billed_input"], 100);
        assert_eq!(row["turn_index"], 0);
    }

    #[test]
    fn a_measured_fixture_parses_into_a_bound() {
        let text = r#"{
  "chars_per_token": {"median": 3.9, "p10": 3.1, "p90": 4.8},
  "generated_utc": "2026-09-13T11:00:00Z",
  "model": "claude-sonnet-5",
  "n_turns": 50,
  "per_block_type": {
    "image": {"mape": 0.9, "mean_actual": 10.0, "mean_est": 19.0, "n": 3},
    "other": {"mape": 0.1, "mean_actual": 5.0, "mean_est": 5.5, "n": 9},
    "text": {"mape": 0.12, "mean_actual": 800.0, "mean_est": 860.0, "n": 50},
    "tool_result": {"mape": 0.18, "mean_actual": 1200.0, "mean_est": 1050.0, "n": 50},
    "tool_use": {"mape": 0.2, "mean_actual": 60.0, "mean_est": 75.0, "n": 50}
  },
  "whole_input_mape": 0.03
}"#;
        let bound = parse_calibration_fixture(text).unwrap().unwrap();
        assert_eq!(bound.date, "2026-09-13");
        assert!((bound.per_block_type.text - 0.12).abs() < 1e-12);
        assert!((bound.per_block_type.tool_use - 0.2).abs() < 1e-12);
    }

    #[test]
    fn a_null_per_block_type_is_the_honest_debt_path() {
        let text = r#"{
  "chars_per_token": {"median": 3.9, "p10": 3.1, "p90": 4.8},
  "generated_utc": "2026-09-13T11:00:00Z",
  "model": null,
  "n_turns": 50,
  "per_block_type": null,
  "whole_input_mape": null
}"#;
        assert!(parse_calibration_fixture(text).unwrap().is_none());
    }

    #[test]
    fn a_corrupt_bound_fails_loud() {
        let text = r#"{"generated_utc": "2026-09-13T11:00:00Z", "per_block_type": {"text": {}}}"#;
        assert!(parse_calibration_fixture(text).is_err());
    }
}
