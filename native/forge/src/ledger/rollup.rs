//! `forge ledger rollup`: design step 2 (`docs/design/cost_ledger.md`
//! sections 2, 3, 6 step 2). Turns the reader's per-file summaries (step 1,
//! PR #35) into the three derived tables step 2 owns: `turn_attribution`,
//! `session_rollup`, `hook_rollup`. `agent_rollup` (step 4, needs the
//! `Agent:` trailer join) and calibration (step 3) are out of scope here.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::Args;
use serde::Serialize;
use serde_json::Value;

use super::reader;
use super::schema::{BlockTypeCounts, CompactionMarker, HookStats, InputKind, TurnSummary};
use super::writer;

/// Chars-per-token used only where a token count must be turned back into an
/// approximate byte count (`resend_bytes` below) -- the same constant this
/// repo already uses for that exact conversion
/// (`src/tooling/hooks/agent/read_budget.py::CHARS_PER_TOKEN`), reused here
/// rather than inventing a second one. The `estimated_tokens_by_block_type`
/// split itself does not need it (it redistributes an already-known token
/// total by byte *share*, not by an absolute bytes-to-tokens conversion);
/// it is folded into `estimate_method` anyway so a reader of one row can see
/// which assumption every number in it rests on, matching design section
/// 3.4's "never print an uncalibrated number silently" rule -- silence is
/// what is forbidden, not an uncalibrated number that names itself as one.
const CHARS_PER_TOKEN: f64 = 4.0;

const ESTIMATE_METHOD: &str = "byte_proportional_uncalibrated_cpt4";

/// The calibration fixture (design step 3), embedded at build time so the
/// bound a row claims is the bound the fixture tests froze -- no runtime
/// path that could drift from it. When the fixture carries no measured
/// `per_block_type` (the no-API-key debt path), rows keep the uncalibrated
/// method string above and a `null` error rather than dressing an
/// unmeasured split up as a calibrated one.
const CALIBRATION_FIXTURE_JSON: &str = include_str!("../../tests/fixtures/ledger/calibration.json");

const DEFAULT_LEDGER_ROOT: &str = "/mnt/data/llm/ledger/";

#[derive(Args)]
pub struct RollupArgs {
    /// Transcript/telemetry file(s), or a directory of `*.jsonl` files
    /// (walked non-recursively; `agent-*.jsonl` subagent transcripts count
    /// as their own sessions, no special-cased handling).
    pub paths: Vec<PathBuf>,

    /// Ledger root directory; defaults to `LEDGER_ROOT`, else
    /// `/mnt/data/llm/ledger/`.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Print the rows to stdout as JSONL and write nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Landed-PR git repo to join `session_rollup` against (design step 4).
    /// When absent, `agent_rollup` is not computed at all -- steps 1-2's
    /// three tables behave exactly as before this flag existed.
    #[arg(long)]
    pub repo: Option<PathBuf>,

    /// Project name the `--repo` scan's commits join against (must match
    /// the `session_rollup` rows' own `project` field, i.e. the transcript
    /// directory's basename `project_of()` already derives). Required with
    /// `--repo`; there is no default because guessing it wrong would either
    /// silently drop every join or -- far worse -- cross a project
    /// boundary, which this design forbids outright.
    #[arg(long)]
    pub project: Option<String>,

    /// Per-session token cap for `agent_rollup.cap_breaches` (design step
    /// 4, `--cap`).
    #[arg(long, default_value_t = super::agent::DEFAULT_CAP)]
    pub cap: u64,

    /// `--since`/`--last` passed straight through to `forge ledger landed`
    /// when `--repo` is given (both optional; omit both to scan every
    /// first-parent commit on `main`).
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub last: Option<u64>,
}

/// One `turn_attribution` row: `TurnSummary`'s own fields (same order) plus
/// the three this step adds. Field order matches the design's name order for
/// the `TurnSummary`-covered fields; `tier` stays absent (step 4).
#[derive(Debug, Clone, Serialize)]
pub struct TurnAttributionRow {
    pub session_id: Option<String>,
    pub turn_index: usize,
    pub turn_uuid: String,
    pub timestamp: Option<String>,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub bytes_by_block_type: BlockTypeCounts,
    pub estimated_tokens_by_block_type: EstimatedTokensByBlockType,
    pub estimate_method: String,
    /// The measured per-block-type MAPE from the embedded calibration
    /// fixture (design step 3), `null` when no bound was measured -- a bare
    /// estimated number is what design section 3.4 forbids, so the error
    /// travels beside the estimate everywhere the estimate goes.
    pub estimate_error_pct: Option<super::calibrate::ErrorByBlockType>,
}

/// `usage.input_tokens + cache_read_input_tokens + cache_creation_input_tokens`
/// redistributed across block types by byte share (design section 3, steps
/// 1-2). `thinking` is excluded: it is never billed as input on the *next*
/// turn (design section 3, step 1), so it gets no share of this turn's input
/// tokens. All zero when the turn has no chars in any included block (no
/// split possible), never a fabricated non-zero guess.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct EstimatedTokensByBlockType {
    pub text: f64,
    pub tool_result: f64,
    pub tool_use: f64,
    pub image: f64,
    pub other: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionRollupRow {
    pub session_id: String,
    pub project: String,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    pub n_turns: u64,
    pub n_compactions: u64,
    pub total_input: u64,
    pub total_output: u64,
    pub total_cache_read: u64,
    pub total_cache_creation: u64,
    /// Sum of `cache_creation_input_tokens * CHARS_PER_TOKEN` over turns
    /// where the turn's own byte total did not grow past the previous
    /// turn's -- an approximation of "paid to re-cache bytes already seen"
    /// (design section 1.3, section 2), not a measurement: the reader never
    /// keeps enough of a turn's actual content to prove byte-identity.
    pub resend_bytes: u64,
    pub resend_events: u64,
    /// `TranscriptSummary::harness_session_ids` (design step 4 join key)
    /// carried through unchanged: a transcript *file* already is one
    /// session in every real case (main-session and `agent-*.jsonl`
    /// transcripts alike), so the file-wide set the reader already computed
    /// is this session's set too -- no second regex pass needed. If a file
    /// ever mixed two `session_id`s (not observed), both sessions built
    /// from it would carry the same superset rather than under-attributing
    /// either.
    pub harness_session_ids: Vec<String>,
    /// Distinct `TurnSummary::model` values seen on this session's turns,
    /// sorted (design step 4, `agent_rollup`'s tier inference input).
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookRollupRow {
    pub hook_name: String,
    pub event: String,
    pub n_calls: u64,
    pub p50_ms: Option<f64>,
    pub p90_ms: Option<f64>,
    pub total_output_bytes: u64,
}

struct RollupOutput {
    turn_attribution: Vec<(String, TurnAttributionRow)>, // (day, row)
    session_rollup: Vec<(String, SessionRollupRow)>,
    hook_rollup: Vec<(String, HookRollupRow)>,
    /// `(day, row)` too, for the same idempotent-write-by-day machinery as
    /// the other three tables, even though every row in one invocation
    /// shares today's date (an aggregate has no single "day it happened").
    agent_rollup: Vec<(String, super::agent::AgentRollupRow)>,
}

pub fn run(args: RollupArgs) -> Result<i32> {
    if args.paths.is_empty() {
        bail!("forge ledger rollup: pass at least one transcript/telemetry file or directory");
    }
    let files = resolve_inputs(&args.paths)?;
    if files.is_empty() {
        bail!("forge ledger rollup: no *.jsonl files found under the given path(s)");
    }

    if args.repo.is_some() && args.project.is_none() {
        bail!("forge ledger rollup: --repo requires --project");
    }

    let mut output = RollupOutput {
        turn_attribution: Vec::new(),
        session_rollup: Vec::new(),
        hook_rollup: Vec::new(),
        agent_rollup: Vec::new(),
    };
    let today = today_utc_date();

    for path in &files {
        match reader::detect_kind(path)? {
            InputKind::Transcript => {
                let summary = reader::read_transcript_file(path)
                    .with_context(|| format!("reading transcript {}", path.display()))?;
                let project = project_of(path);
                process_transcript(&summary, &project, &mut output)?;
            }
            InputKind::Telemetry => {
                let summary = reader::read_telemetry_file(path)
                    .with_context(|| format!("reading telemetry {}", path.display()))?;
                for hook in &summary.hooks {
                    output.hook_rollup.push((today.clone(), hook_row(hook)));
                }
            }
        }
    }

    if let Some(repo) = &args.repo {
        let project = args.project.as_deref().expect("checked above");
        let commits = super::landed::scan(repo, args.since.as_deref(), args.last)
            .with_context(|| format!("scanning landed commits in {}", repo.display()))?;
        let sessions: Vec<SessionRollupRow> = output
            .session_rollup
            .iter()
            .map(|(_, row)| row.clone())
            .collect();
        let (agent_rows, joins) =
            super::agent::build_agent_rollup(&commits, &sessions, project, args.cap);
        let unjoined = super::agent::unjoined_commit_count(&joins);
        if unjoined > 0 {
            eprintln!(
                "forge ledger rollup: {unjoined}/{} landed commits have no joined session (design step 4 finding, not tuned away):",
                joins.len()
            );
            for join in joins.iter().filter(|j| j.join_method == "unjoined") {
                eprintln!(
                    "  {} pr={}",
                    join.sha,
                    join.pr_number
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "-".to_string())
                );
            }
        }
        let ambiguous = joins.iter().filter(|j| j.ambiguous).count();
        if ambiguous > 0 {
            eprintln!(
                "forge ledger rollup: {ambiguous} landed commit(s) matched more than one session on the time-window fallback (agent_rollup.join_method=\"time_window\"; every candidate was credited)"
            );
        }
        for row in agent_rows {
            output.agent_rollup.push((today.clone(), row));
        }
    }

    if args.dry_run {
        print_dry_run(&output)?;
        return Ok(0);
    }

    let ledger_root = resolve_ledger_root(args.out);
    write_all(&ledger_root, &output)?;
    Ok(0)
}

/// Non-recursive: a directory argument contributes its immediate `*.jsonl`
/// children only (design step 2, item 3) -- a subagent transcript
/// (`agent-*.jsonl`) sits beside its parent's file and is walked the same
/// way, no special case.
fn resolve_inputs(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in paths {
        let meta = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
        if meta.is_dir() {
            let mut entries: Vec<PathBuf> = fs::read_dir(path)
                .with_context(|| format!("reading directory {}", path.display()))?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
                .collect();
            entries.sort();
            files.extend(entries);
        } else {
            files.push(path.clone());
        }
    }
    Ok(files)
}

fn project_of(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn process_transcript(
    summary: &super::schema::TranscriptSummary,
    project: &str,
    output: &mut RollupOutput,
) -> Result<()> {
    let mut turns_by_session: BTreeMap<String, Vec<&TurnSummary>> = BTreeMap::new();
    for turn in &summary.turns {
        let Some(session_id) = turn.session_id.clone() else {
            bail!(
                "{}: turn {} has no session_id -- cannot attribute or roll it up",
                summary.path,
                turn.turn_uuid
            );
        };
        turns_by_session.entry(session_id).or_default().push(turn);
    }
    let mut markers_by_session: BTreeMap<String, Vec<&CompactionMarker>> = BTreeMap::new();
    for marker in &summary.compaction_markers {
        if let Some(session_id) = &marker.session_id {
            markers_by_session
                .entry(session_id.clone())
                .or_default()
                .push(marker);
        }
    }

    let mut sessions: BTreeSet<String> = turns_by_session.keys().cloned().collect();
    sessions.extend(markers_by_session.keys().cloned());

    for session_id in sessions {
        let turns = turns_by_session
            .get(&session_id)
            .cloned()
            .unwrap_or_default();
        let markers = markers_by_session
            .get(&session_id)
            .cloned()
            .unwrap_or_default();

        for turn in &turns {
            let row = turn_attribution_row(turn);
            let day = day_of_timestamp(turn.timestamp.as_deref()).with_context(|| {
                format!(
                    "{}: turn {} has no usable timestamp to bucket into a day file",
                    summary.path, turn.turn_uuid
                )
            })?;
            output.turn_attribution.push((day, row));
        }

        if turns.is_empty() {
            // A session with only compaction markers and no usage-bearing
            // turn (e.g. a summary line at the very start of a file) still
            // gets no `session_rollup` row -- there is nothing to total.
            continue;
        }
        let row = session_rollup_row(
            &session_id,
            project,
            &turns,
            &markers,
            &summary.harness_session_ids,
        );
        let day = day_of_timestamp(row.first_ts.as_deref()).with_context(|| {
            format!(
                "{}: session {session_id} has no usable timestamp for its session_rollup day",
                summary.path
            )
        })?;
        output.session_rollup.push((day, row));
    }
    Ok(())
}

/// The embedded calibration bound, parsed once. `None` when the fixture
/// shipped without a measured `per_block_type` (the offline debt path) --
/// the row label then stays uncalibrated and the error field `null`.
fn calibration() -> Option<&'static super::calibrate::CalibrationBound> {
    static BOUND: std::sync::OnceLock<Option<super::calibrate::CalibrationBound>> =
        std::sync::OnceLock::new();
    BOUND
        .get_or_init(|| {
            super::calibrate::parse_calibration_fixture(CALIBRATION_FIXTURE_JSON).expect(
                "the committed calibration fixture must parse; a corrupt bound stops the build",
            )
        })
        .as_ref()
}

fn turn_attribution_row(turn: &TurnSummary) -> TurnAttributionRow {
    build_turn_attribution_row(turn, calibration())
}

/// The row builder takes the bound as a parameter (the caller passes the
/// embedded fixture's parse) so the calibrated and uncalibrated labels are
/// both unit-testable without rewriting the committed fixture.
fn build_turn_attribution_row(
    turn: &TurnSummary,
    bound: Option<&super::calibrate::CalibrationBound>,
) -> TurnAttributionRow {
    TurnAttributionRow {
        session_id: turn.session_id.clone(),
        turn_index: turn.turn_index,
        turn_uuid: turn.turn_uuid.clone(),
        timestamp: turn.timestamp.clone(),
        model: turn.model.clone(),
        input_tokens: turn.input_tokens,
        output_tokens: turn.output_tokens,
        cache_read_input_tokens: turn.cache_read_input_tokens,
        cache_creation_input_tokens: turn.cache_creation_input_tokens,
        bytes_by_block_type: turn.bytes_by_block_type,
        estimated_tokens_by_block_type: estimate_tokens_by_block_type(turn),
        estimate_method: match bound {
            Some(bound) => format!("byte_proportional_calibrated_{}", bound.date),
            None => ESTIMATE_METHOD.to_string(),
        },
        estimate_error_pct: bound.map(|bound| bound.per_block_type),
    }
}

/// Design section 3: redistribute this turn's total billed input tokens
/// (`input_tokens + cache_read_input_tokens + cache_creation_input_tokens`)
/// across blocks by their share of this turn's chars, excluding `thinking`.
/// Every split field is a char count in the same unit since the calibration
/// step's reader fix (`tool_use` = serialized `input` + tool name, `image`
/// = base64 payload), so the proportional denominator mixes no block counts.
fn estimate_tokens_by_block_type(turn: &TurnSummary) -> EstimatedTokensByBlockType {
    let b = &turn.bytes_by_block_type;
    let total_tokens =
        turn.input_tokens + turn.cache_read_input_tokens + turn.cache_creation_input_tokens;
    let total_chars = b.text + b.tool_result + b.tool_use + b.image + b.other;
    if total_tokens == 0 || total_chars == 0 {
        return EstimatedTokensByBlockType::default();
    }
    let ratio = total_tokens as f64 / total_chars as f64;
    EstimatedTokensByBlockType {
        text: b.text as f64 * ratio,
        tool_result: b.tool_result as f64 * ratio,
        tool_use: b.tool_use as f64 * ratio,
        image: b.image as f64 * ratio,
        other: b.other as f64 * ratio,
    }
}

/// Design section 2 "Compaction detection": primary signal is the
/// `isCompactSummary` marker (one line per compaction, unambiguous); the
/// `cache_read_input_tokens` drop heuristic is used only when a session has
/// no marker at all (older transcripts, or a harness version that did not
/// write one) so the two signals are never summed for the same event --
/// summing would double-count the marker and the drop it causes as two
/// compactions instead of one.
fn count_compactions(turns: &[&TurnSummary], markers: &[&CompactionMarker]) -> u64 {
    if !markers.is_empty() {
        return markers.len() as u64;
    }
    let mut count = 0u64;
    for pair in turns.windows(2) {
        let prev = pair[0].cache_read_input_tokens;
        let cur = pair[1].cache_read_input_tokens;
        if prev > 0 && (cur as f64) * 2.0 < prev as f64 {
            count += 1;
        }
    }
    count
}

/// Design section 2 "Resend": a turn's own `bytes_by_block_type` total is
/// the proxy for "new resident content introduced this turn"; when it does
/// not exceed the previous turn's total but the turn still paid
/// `cache_creation_input_tokens > 0`, that payment is treated as re-caching
/// bytes already seen rather than genuinely new ones (approximation, per
/// design -- the reader never retains actual bytes to prove identity).
fn resend(turns: &[&TurnSummary]) -> (u64, u64) {
    let mut resend_bytes = 0u64;
    let mut resend_events = 0u64;
    for pair in turns.windows(2) {
        let prev_total = block_total(&pair[0].bytes_by_block_type);
        let cur_total = block_total(&pair[1].bytes_by_block_type);
        let cache_creation = pair[1].cache_creation_input_tokens;
        if cur_total <= prev_total && cache_creation > 0 {
            resend_events += 1;
            resend_bytes += (cache_creation as f64 * CHARS_PER_TOKEN).round() as u64;
        }
    }
    (resend_bytes, resend_events)
}

fn block_total(b: &BlockTypeCounts) -> u64 {
    b.text + b.tool_result + b.tool_use + b.thinking + b.image + b.other
}

fn session_rollup_row(
    session_id: &str,
    project: &str,
    turns: &[&TurnSummary],
    markers: &[&CompactionMarker],
    harness_session_ids: &[String],
) -> SessionRollupRow {
    let mut first_ts: Option<String> = None;
    let mut last_ts: Option<String> = None;
    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cache_read = 0u64;
    let mut total_cache_creation = 0u64;
    for turn in turns {
        if let Some(ts) = &turn.timestamp {
            if first_ts.as_deref().is_none_or(|f| ts.as_str() < f) {
                first_ts = Some(ts.clone());
            }
            if last_ts.as_deref().is_none_or(|l| ts.as_str() > l) {
                last_ts = Some(ts.clone());
            }
        }
        total_input += turn.input_tokens;
        total_output += turn.output_tokens;
        total_cache_read += turn.cache_read_input_tokens;
        total_cache_creation += turn.cache_creation_input_tokens;
    }
    for marker in markers {
        if let Some(ts) = &marker.timestamp {
            if first_ts.as_deref().is_none_or(|f| ts.as_str() < f) {
                first_ts = Some(ts.clone());
            }
            if last_ts.as_deref().is_none_or(|l| ts.as_str() > l) {
                last_ts = Some(ts.clone());
            }
        }
    }
    let (resend_bytes, resend_events) = resend(turns);
    let models: Vec<String> = turns
        .iter()
        .filter_map(|turn| turn.model.clone())
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    SessionRollupRow {
        session_id: session_id.to_string(),
        project: project.to_string(),
        first_ts,
        last_ts,
        n_turns: turns.len() as u64,
        n_compactions: count_compactions(turns, markers),
        total_input,
        total_output,
        total_cache_read,
        total_cache_creation,
        resend_bytes,
        resend_events,
        harness_session_ids: harness_session_ids.to_vec(),
        models,
    }
}

fn hook_row(hook: &HookStats) -> HookRollupRow {
    HookRollupRow {
        hook_name: hook.hook.clone(),
        event: hook.event.clone(),
        n_calls: hook.n_calls,
        p50_ms: hook.p50_ms,
        p90_ms: hook.p90_ms,
        total_output_bytes: hook.total_output_bytes,
    }
}

/// First 10 characters of an RFC3339 timestamp (`YYYY-MM-DD`). Fails loud
/// (`None`) rather than guessing a day for a row this reader cannot place --
/// every real transcript line carries a timestamp; a missing one is a
/// malformed-input condition, not a silent-default one.
fn day_of_timestamp(ts: Option<&str>) -> Result<String> {
    let ts = ts.context("missing timestamp")?;
    if ts.len() < 10 {
        bail!("timestamp {ts:?} is too short to hold a YYYY-MM-DD date");
    }
    Ok(ts[..10].to_string())
}

/// `hook_rollup` rows carry no per-call timestamp (`HookStats` is already an
/// aggregate with none) -- there is no "the day this data represents" to
/// derive, so it is filed under the day the rollup command ran instead.
pub(crate) fn today_utc_date() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        / 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days-since-epoch (1970-01-01) to a proleptic Gregorian (year, month, day).
/// Duplicated from `crate::civil::civil_from_days` (same algorithm, same
/// source: Howard Hinnant's civil-calendar algorithms) rather than imported:
/// `ledger/mod.rs` is compiled standalone via `#[path]` in
/// `tests/ledger_fixtures.rs` (this binary crate has no lib target), which
/// cannot see sibling top-level modules like `civil`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn resolve_ledger_root(out: Option<PathBuf>) -> PathBuf {
    out.or_else(|| std::env::var("LEDGER_ROOT").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LEDGER_ROOT))
}

fn print_dry_run(output: &RollupOutput) -> Result<()> {
    for (_, row) in &output.turn_attribution {
        println!("{}", serde_json::to_string(row)?);
    }
    for (_, row) in &output.session_rollup {
        println!("{}", serde_json::to_string(row)?);
    }
    for (_, row) in &output.hook_rollup {
        println!("{}", serde_json::to_string(row)?);
    }
    for (_, row) in &output.agent_rollup {
        println!("{}", serde_json::to_string(row)?);
    }
    Ok(())
}

fn write_all(ledger_root: &Path, output: &RollupOutput) -> Result<()> {
    write_table(
        ledger_root,
        "turn_attribution",
        "session_id",
        &output.turn_attribution,
    )?;
    write_table(
        ledger_root,
        "session_rollup",
        "session_id",
        &output.session_rollup,
    )?;
    write_table(ledger_root, "hook_rollup", "hook_name", &output.hook_rollup)?;
    write_table(
        ledger_root,
        "agent_rollup",
        "agent_name",
        &output.agent_rollup,
    )?;
    Ok(())
}

fn write_table<T: Serialize>(
    ledger_root: &Path,
    table: &str,
    key_field: &str,
    rows: &[(String, T)],
) -> Result<()> {
    let mut by_day: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for (day, row) in rows {
        by_day
            .entry(day.as_str())
            .or_default()
            .push(serde_json::to_value(row)?);
    }
    for (day, day_rows) in by_day {
        writer::write_day_file(ledger_root, table, day, key_field, &day_rows)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn() -> TurnSummary {
        TurnSummary {
            session_id: Some("s".to_string()),
            turn_index: 0,
            turn_uuid: "u".to_string(),
            timestamp: Some("2026-09-13T00:00:00Z".to_string()),
            model: Some("claude-sonnet-5".to_string()),
            input_tokens: 100,
            output_tokens: 1,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            bytes_by_block_type: BlockTypeCounts {
                text: 60,
                tool_result: 40,
                ..BlockTypeCounts::default()
            },
        }
    }

    /// No bound (the committed debt fixture, or none at all): the row keeps
    /// the uncalibrated label and a `null` error -- design 3.4's rule that
    /// an estimate never travels without its error *or* its named absence.
    #[test]
    fn without_a_bound_the_row_stays_honestly_uncalibrated() {
        let row = build_turn_attribution_row(&turn(), None);
        assert_eq!(row.estimate_method, "byte_proportional_uncalibrated_cpt4");
        assert!(row.estimate_error_pct.is_none());
        assert_eq!(row.estimated_tokens_by_block_type.text, 60.0);
    }

    /// A measured bound renames the method after its measurement date and
    /// carries the per-block error beside the estimate.
    #[test]
    fn a_measured_bound_labels_and_stamps_the_row() {
        let bound = super::super::calibrate::parse_calibration_fixture(
            r#"{"generated_utc": "2026-09-13T00:00:00Z",
                "per_block_type": {
                  "text": {"mape": 0.11, "n": 50, "mean_est": 1.0, "mean_actual": 1.0},
                  "tool_result": {"mape": 0.22, "n": 50, "mean_est": 1.0, "mean_actual": 1.0},
                  "tool_use": {"mape": 0.33, "n": 50, "mean_est": 1.0, "mean_actual": 1.0},
                  "image": {"mape": 0.44, "n": 3, "mean_est": 1.0, "mean_actual": 1.0},
                  "other": {"mape": 0.55, "n": 9, "mean_est": 1.0, "mean_actual": 1.0}
                }}"#,
        )
        .unwrap()
        .unwrap();
        let row = build_turn_attribution_row(&turn(), Some(&bound));
        assert_eq!(
            row.estimate_method,
            "byte_proportional_calibrated_2026-09-13"
        );
        let error = row.estimate_error_pct.expect("bound present");
        assert!((error.text - 0.11).abs() < 1e-12);
        assert!((error.other - 0.55).abs() < 1e-12);
    }
}
