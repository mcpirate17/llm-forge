//! Branch policy: naming rules, binding-store validation, and the push-decision core.
//!
//! Python (`conductor/branch_policy.py`) keeps subprocess git, the clock, the CLI and
//! filesystem policy, exactly as `git_source.rs` draws the boundary. This module owns
//! everything deterministic that the pre-push hook consults: the branch-name grammar
//! and its refusal messages, best-effort name suggestions, force-mode token
//! classification, ISO-timestamp parsing for binding staleness, the binding-store
//! schema, the one-claim-one-live-branch fan-out guard, and the reason construction
//! for both push classes. The refusal and reason strings are load-bearing: the
//! pre-push hook prints them at agents, and `conductor/test_branch_policy.py` asserts
//! them, so they are ported character-for-character.

use std::collections::HashSet;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use serde_json::Value;

const EXPECTED_SHAPE: &str = "expected shape: <agent>/<topic>-<yyyymmdd> \
     (agent=[a-z][a-z0-9-]*, topic=[a-z0-9][a-z0-9-]*, date=8-digit valid calendar date)";

/// Empty-store JSON handed to the native helpers when no store file exists, mirroring
/// `load_bindings`' missing-file case without a second filesystem read in Python.
const EMPTY_STORE: &str = r#"{"schema_version":1,"bindings":[]}"#;

// --------------------------------------------------------------------------- repr

/// `repr()` of a Python string: quote style follows Python's rule (single quotes
/// unless the text contains `'` and not `"`), and the escapes cover the control
/// characters a branch name can realistically carry. Deep-unicode repr parity is out
/// of scope; branch names are filesystem-ish tokens, not prose.
fn python_repr(s: &str) -> String {
    let use_double = s.contains('\'') && !s.contains('"');
    let mut out = String::with_capacity(s.len() + 2);
    out.push(if use_double { '"' } else { '\'' });
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\'' if use_double => out.push('\''),
            '"' if !use_double => out.push('"'),
            '"' => out.push_str("\\\""),
            '\'' => out.push_str("\\'"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(if use_double { '"' } else { '\'' });
    out
}

// --------------------------------------------------------------------------- naming

/// Python's `re.sub(r"[^a-z0-9-]+", "-", raw.strip().lower())`, then strip and
/// collapse runs of `-`. `trim` removes Unicode whitespace, matching `str.strip()`.
fn slugify(raw: &str) -> String {
    let lowered = raw.trim().to_lowercase();
    let substituted: String = lowered
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    substituted
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn is_agent_slug(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {
            chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        }
        _ => false,
    }
}

fn is_topic_slug(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {
            chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        }
        _ => false,
    }
}

/// Trailing 8 ASCII digits of a string, with the char-safe start index — Python's
/// `re.search(r"(\d{8})$", rest)`. Char-based because branch names are user text and
/// a byte index here would not even be a char boundary.
fn trailing_date(rest: &str) -> Option<(String, String)> {
    let chars: Vec<char> = rest.chars().collect();
    if chars.len() < 8 {
        return None;
    }
    let start = chars.len() - 8;
    if !chars[start..].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let date: String = chars[start..].iter().collect();
    let prefix: String = chars[..start].iter().collect();
    Some((prefix, date))
}

/// `datetime.strptime(date, "%Y%m%d")` validity: 8 digits forming a real calendar day
/// with a year Python's datetime can represent (1..=9999; year 0 is out of range).
fn valid_calendar_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    if bytes.len() != 8 || !bytes.iter().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let year: i64 = date[..4].parse().unwrap_or(0);
    let month: i64 = date[4..6].parse().unwrap_or(0);
    let day: i64 = date[6..].parse().unwrap_or(0);
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let dim = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if leap => 29,
        _ => 28,
    };
    (1..=dim).contains(&day)
}

fn suggest_branch_name(name: &str, today: &str) -> String {
    let (agent_raw, rest_raw) = match name.split_once('/') {
        Some((agent, rest)) => (agent, rest),
        None => (name, ""),
    };
    let mut agent = match slugify(agent_raw) {
        slug if slug.is_empty() => "agent".to_owned(),
        slug => slug,
    };
    let first = agent.chars().next().unwrap_or('a');
    if !first.is_ascii_alphabetic() {
        agent = format!("a{agent}");
    }
    let (date, topic_raw) = match trailing_date(rest_raw) {
        Some((prefix, date)) if valid_calendar_date(&date) => {
            (date, prefix.trim_end_matches('-').to_owned())
        }
        _ => (today.to_owned(), rest_raw.to_owned()),
    };
    let topic = match slugify(&topic_raw) {
        slug if slug.is_empty() => "topic".to_owned(),
        slug => slug,
    };
    format!("{agent}/{topic}-{date}")
}

/// `(raw, agent, topic, date)` for a valid name, or the exact refusal message the
/// Python module raised. Each violated rule keeps its distinct message so callers can
/// tell which check fired.
fn validate_name(name: &str, today: &str) -> Result<(String, String, String, String), String> {
    let suggestion = suggest_branch_name(name, today);
    let Some((agent, rest)) = name.split_once('/') else {
        return Err(format!(
            "branch name {} has no '<agent>/' segment; {EXPECTED_SHAPE}; try {}",
            python_repr(name),
            python_repr(&suggestion),
        ));
    };
    if !is_agent_slug(agent) {
        return Err(format!(
            "branch name {} has an invalid agent slug {}; {EXPECTED_SHAPE}; try {}",
            python_repr(name),
            python_repr(agent),
            python_repr(&suggestion),
        ));
    }
    // `<topic>-<yyyymmdd>` with the regex's greedy split: the final `-` before exactly
    // 8 digits is the separator, and whatever precedes it must be a topic slug.
    let tail = rest.rsplit_once('-').filter(|(topic, date)| {
        date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) && is_topic_slug(topic)
    });
    let Some((topic, date)) = tail else {
        return Err(format!(
            "branch name {} has no '<topic>-<yyyymmdd>' segment after '{}'; \
             {EXPECTED_SHAPE}; try {}",
            python_repr(name),
            agent,
            python_repr(&suggestion),
        ));
    };
    if !valid_calendar_date(date) {
        return Err(format!(
            "branch name {} has an invalid calendar date {} (yyyymmdd); \
             {EXPECTED_SHAPE}; try {}",
            python_repr(name),
            python_repr(date),
            python_repr(&suggestion),
        ));
    }
    Ok((
        name.to_owned(),
        agent.to_owned(),
        topic.to_owned(),
        date.to_owned(),
    ))
}

// --------------------------------------------------------------------------- force mode

fn force_mode_from_tokens(tokens: &[String]) -> String {
    if !tokens.iter().any(|t| t == "push") {
        return "unknown".to_owned();
    }
    if tokens
        .iter()
        .any(|t| t == "--force-with-lease" || t.starts_with("--force-with-lease="))
    {
        return "lease".to_owned();
    }
    if tokens.iter().any(|t| t == "--force" || t == "-f") {
        return "bare".to_owned();
    }
    "none".to_owned()
}

// --------------------------------------------------------------------------- stamps

/// Civil-date to days since the epoch (Howard Hinnant's algorithm); valid for the
/// years Python's datetime represents.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn two_digits(s: &[u8], at: usize) -> Option<i64> {
    if s.len() < at + 2 || !s[at..at + 2].iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(i64::from(s[at] - b'0') * 10 + i64::from(s[at + 1] - b'0'))
}

fn valid_ymd(year: i64, month: i64, day: i64) -> bool {
    if !(1..=9999).contains(&year) {
        return false;
    }
    valid_calendar_date(&format!("{year:04}{month:02}{day:02}"))
}

/// Parse an ISO-8601 stamp the way `datetime.fromisoformat` does for the shapes this
/// module writes and reads: `YYYY-MM-DD[THH:MM[:SS[.f{1,6}]]][Z|±HH[:MM[:SS]]]`,
/// including the date-only form. Returns epoch microseconds plus whether the stamp
/// carried a UTC offset; a naive stamp is reported so the Python adapter can raise
/// the same `TypeError` aware-minus-naive subtraction raises.
fn parse_stamp(stamp: &str) -> Result<(i64, bool), ()> {
    let bytes = stamp.as_bytes();
    let invalid = Err(());
    if bytes.len() < 10 {
        return invalid;
    }
    let year: i64 = stamp[..4].parse().map_err(|_| ())?;
    let month = two_digits(bytes, 5).ok_or(())?;
    let day = two_digits(bytes, 8).ok_or(())?;
    if !valid_ymd(year, month, day) {
        return invalid;
    }
    let parse_time = |cursor: &mut usize| -> Result<(i64, i64, i64, i64), ()> {
        let hour = two_digits(bytes, *cursor).ok_or(())?;
        *cursor += 2;
        let minute = if bytes.get(*cursor) == Some(&b':') {
            *cursor += 1;
            let m = two_digits(bytes, *cursor).ok_or(())?;
            *cursor += 2;
            m
        } else {
            0
        };
        let second = if bytes.get(*cursor) == Some(&b':') {
            *cursor += 1;
            let s = two_digits(bytes, *cursor).ok_or(())?;
            *cursor += 2;
            s
        } else {
            0
        };
        let mut micros = 0i64;
        if bytes.get(*cursor) == Some(&b'.') {
            *cursor += 1;
            let start = *cursor;
            while *cursor < bytes.len() && bytes[*cursor].is_ascii_digit() && *cursor - start < 6 {
                *cursor += 1;
            }
            if *cursor == start {
                return Err(());
            }
            let digits = &stamp[start..*cursor];
            micros = format!("{digits:0<6}").parse().map_err(|_| ())?;
        }
        Ok((hour, minute, second, micros))
    };
    let parse_offset = |cursor: &mut usize| -> Result<Option<i64>, ()> {
        match bytes.get(*cursor) {
            None => Ok(None),
            Some(&b'Z') if *cursor + 1 == bytes.len() => Ok(Some(0)),
            Some(sign @ (b'+' | b'-')) => {
                *cursor += 1;
                let off_hour = two_digits(bytes, *cursor).ok_or(())?;
                *cursor += 2;
                let off_minute = if bytes.get(*cursor) == Some(&b':') {
                    *cursor += 1;
                    let m = two_digits(bytes, *cursor).ok_or(())?;
                    *cursor += 2;
                    m
                } else if bytes.get(*cursor).is_some() {
                    let m = two_digits(bytes, *cursor).ok_or(())?;
                    *cursor += 2;
                    m
                } else {
                    0
                };
                if bytes.get(*cursor).is_some() {
                    return Err(());
                }
                if off_hour >= 24 || off_minute >= 60 {
                    return Err(());
                }
                let magnitude = off_hour * 3600 + off_minute * 60;
                Ok(Some(if *sign == b'-' { -magnitude } else { magnitude }))
            }
            Some(_) => Err(()),
        }
    };
    match bytes.get(10) {
        None | Some(&b'T') | Some(&b' ') => {}
        _ => return invalid,
    }
    if bytes.get(10).is_none() {
        // Date-only: midnight, naive.
        return Ok((days_from_civil(year, month, day) * 86_400_000_000, false));
    }
    let mut cursor = 11usize;
    let (hour, minute, second, micros) = parse_time(&mut cursor)?;
    let offset_secs = parse_offset(&mut cursor)?;
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        return invalid;
    }
    match offset_secs {
        None => Ok((
            days_from_civil(year, month, day) * 86_400_000_000
                + (hour * 3600 + minute * 60 + second) * 1_000_000
                + micros,
            false,
        )),
        Some(offset) => {
            let secs =
                days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
                    - offset;
            Ok((secs * 1_000_000 + micros, true))
        }
    }
}

// --------------------------------------------------------------------------- bindings

#[derive(Clone, Debug)]
struct Binding {
    branch: String,
    claim_id: String,
    owner: String,
    created_at: String,
    last_push_at: Option<String>,
    pr_number: Option<i64>,
}

impl Binding {
    fn to_row(&self) -> Value {
        serde_json::json!({
            "branch": self.branch,
            "claim_id": self.claim_id,
            "owner": self.owner,
            "created_at": self.created_at,
            "last_push_at": self.last_push_at,
            "pr_number": self.pr_number,
        })
    }
}

/// `str()` of a JSON scalar the way Python's `str()` renders it after `json.loads`.
/// Containers fall back to serde's rendering; branch/claim/owner fields are strings in
/// every real store, so the divergence is unreachable in practice.
fn python_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(n) => n.to_string(),
        Value::Null => "None".to_owned(),
        other => other.to_string(),
    }
}

/// `isinstance(v, int)` after `json.loads`: JSON integers pass, and so do JSON
/// booleans because Python bools are ints. Stored as 1/0, which is indistinguishable
/// in Python where `True == 1`.
fn python_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

const BINDING_FIELDS: [&str; 6] = [
    "branch",
    "claim_id",
    "owner",
    "created_at",
    "last_push_at",
    "pr_number",
];

fn binding_from_payload(payload: &Value) -> Result<Binding, String> {
    let Some(map) = payload.as_object() else {
        return Err("branch binding has an invalid schema".to_owned());
    };
    let matches: HashSet<&str> = map.keys().map(String::as_str).collect();
    if matches.len() != BINDING_FIELDS.len() || !BINDING_FIELDS.iter().all(|f| matches.contains(f))
    {
        return Err("branch binding has an invalid schema".to_owned());
    }
    let branch = python_str(&payload["branch"]);
    let claim_id = python_str(&payload["claim_id"]);
    let owner = python_str(&payload["owner"]);
    let created_at = python_str(&payload["created_at"]);
    let last_push_at = match &payload["last_push_at"] {
        Value::Null => None,
        v => Some(python_str(v)),
    };
    let pr_number = match &payload["pr_number"] {
        Value::Null => None,
        v => python_int(v).map(Some).ok_or_else(|| {
            format!(
                "binding {} pr_number must be int or null",
                python_repr(&branch)
            )
        })?,
    };
    if branch.is_empty() || claim_id.is_empty() || owner.is_empty() || created_at.is_empty() {
        return Err("branch binding is missing a required field".to_owned());
    }
    Ok(Binding {
        branch,
        claim_id,
        owner,
        created_at,
        last_push_at,
        pr_number,
    })
}

fn validate_store(text: &str) -> Result<Vec<Binding>, String> {
    let invalid_top = || "branch binding store has an invalid top-level schema".to_owned();
    let payload: Value = serde_json::from_str(text).map_err(|_| invalid_top())?;
    let Some(map) = payload.as_object() else {
        return Err(invalid_top());
    };
    let matches: HashSet<&str> = map.keys().map(String::as_str).collect();
    if matches.len() != 2 || !matches.contains("schema_version") || !matches.contains("bindings") {
        return Err(invalid_top());
    }
    // Python compares `payload["schema_version"] != 1`; a JSON `true` equals 1 there
    // because bools are ints, and a JSON string "1" does not.
    let version = &payload["schema_version"];
    let version_ok = matches!(version, Value::Number(n) if n.as_i64() == Some(1))
        || *version == Value::Bool(true);
    let Some(rows) = payload["bindings"].as_array() else {
        return Err("branch binding store schema version or bindings are invalid".to_owned());
    };
    if !version_ok {
        return Err("branch binding store schema version or bindings are invalid".to_owned());
    }
    let bindings = rows
        .iter()
        .map(binding_from_payload)
        .collect::<Result<Vec<_>, _>>()?;
    let mut seen = HashSet::new();
    for binding in &bindings {
        if !seen.insert(binding.branch.as_str()) {
            return Err("branch binding store contains duplicate branches".to_owned());
        }
    }
    Ok(bindings)
}

fn second_branch_conflict(
    live: &[String],
    bindings: &[Binding],
    branch: &str,
    claim_id: &str,
) -> Option<String> {
    bindings
        .iter()
        .find(|b| b.claim_id == claim_id && b.branch != branch && live.contains(&b.branch))
        .map(|b| {
            format!(
                "claim {} is already bound to live branch {}; a second live branch \
                 on the same claim is refused",
                python_repr(claim_id),
                python_repr(&b.branch),
            )
        })
}

// --------------------------------------------------------------------------- decisions

fn feature_push_reasons(
    branch: &str,
    fast_forward: bool,
    force_mode: &str,
    today: &str,
    store_text: Option<&str>,
    live: &[String],
) -> Result<Vec<String>, String> {
    let mut reasons = Vec::new();
    if let Err(message) = validate_name(branch, today) {
        reasons.push(message);
    }
    if !fast_forward {
        if force_mode == "bare" {
            reasons.push(format!(
                "non-fast-forward push to {} used a bare --force; \
                 policy requires --force-with-lease",
                python_repr(branch),
            ));
        } else if force_mode != "lease" {
            let detail = if force_mode == "none" {
                "no force flag was declared".to_owned()
            } else {
                "force mode could not be established from hook context (git's \
                 pre-push protocol exposes no argv/env for it); re-run explicitly as \
                 `check-push --force-with-lease` once lease safety is confirmed"
                    .to_owned()
            };
            reasons.push(format!(
                "non-fast-forward push to {} is refused by default: {detail}",
                python_repr(branch),
            ));
        }
    }
    let bindings = validate_store(store_text.unwrap_or(EMPTY_STORE))?;
    if let Some(binding) = bindings.iter().find(|b| b.branch == branch) {
        if let Some(conflict) = second_branch_conflict(live, &bindings, branch, &binding.claim_id) {
            reasons.push(conflict);
        }
    }
    Ok(reasons)
}

fn integration_push_reasons(
    branch: &str,
    remote_old: &str,
    remote_new: &str,
    fast_forward: bool,
    missing: &[String],
) -> Vec<String> {
    if !fast_forward {
        let old_prefix = if remote_old.is_empty() {
            "(new)".to_owned()
        } else {
            remote_old.chars().take(8).collect()
        };
        return vec![format!(
            "integration branch {} requires a fast-forward push; {} is not an ancestor of {}",
            python_repr(branch),
            old_prefix,
            remote_new.chars().take(8).collect::<String>(),
        )];
    }
    if missing.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "integration branch {} would carry {} commit(s) not already present on any other \
         pushed ref: {}",
        python_repr(branch),
        missing.len(),
        missing
            .iter()
            .take(5)
            .map(|sha| sha.chars().take(8).collect::<String>())
            .collect::<Vec<_>>()
            .join(", "),
    )]
}

// --------------------------------------------------------------------------- pyo3

#[pyfunction]
fn branch_policy_validate_name_native(
    name: &str,
    today: &str,
) -> PyResult<(String, String, String, String)> {
    validate_name(name, today).map_err(PyValueError::new_err)
}

#[pyfunction]
fn branch_policy_suggest_name_native(name: &str, today: &str) -> String {
    suggest_branch_name(name, today)
}

#[pyfunction]
fn branch_policy_force_mode_from_tokens_native(tokens: Vec<String>) -> String {
    force_mode_from_tokens(&tokens)
}

/// Hours between ``stamp`` and ``now`` as f64. Invalid stamps raise ``ValueError``
/// (matching ``datetime.fromisoformat``); a naive/aware mix raises ``TypeError``
/// (matching aware-minus-naive subtraction).
#[pyfunction]
fn branch_policy_stamp_age_hours_native(stamp: &str, now: &str) -> PyResult<f64> {
    let (stamp_us, stamp_aware) = parse_stamp(stamp)
        .map_err(|_| PyValueError::new_err(format!("Invalid isoformat string: {stamp:?}")))?;
    let (now_us, now_aware) = parse_stamp(now)
        .map_err(|_| PyValueError::new_err(format!("Invalid isoformat string: {now:?}")))?;
    if stamp_aware != now_aware {
        return Err(PyTypeError::new_err(
            "can't subtract offset-naive and offset-aware datetimes",
        ));
    }
    Ok((now_us - stamp_us) as f64 / 3_600_000_000.0)
}

#[pyfunction]
fn branch_policy_validate_bindings_native(text: &str) -> PyResult<String> {
    let bindings = validate_store(text).map_err(PyValueError::new_err)?;
    let rows: Vec<Value> = bindings.iter().map(Binding::to_row).collect();
    serde_json::to_string(&rows).map_err(|err| PyValueError::new_err(err.to_string()))
}

#[pyfunction]
fn branch_policy_second_branch_conflict_native(
    live_branches: Vec<String>,
    store_text: Option<&str>,
    branch: &str,
    claim_id: &str,
) -> PyResult<Option<String>> {
    let bindings =
        validate_store(store_text.unwrap_or(EMPTY_STORE)).map_err(PyValueError::new_err)?;
    Ok(second_branch_conflict(
        &live_branches,
        &bindings,
        branch,
        claim_id,
    ))
}

#[pyfunction]
fn branch_policy_evaluate_feature_push_native(
    branch: &str,
    fast_forward: bool,
    force_mode: &str,
    today: &str,
    store_text: Option<&str>,
    live_branches: Vec<String>,
) -> PyResult<Vec<String>> {
    feature_push_reasons(
        branch,
        fast_forward,
        force_mode,
        today,
        store_text,
        &live_branches,
    )
    .map_err(PyValueError::new_err)
}

#[pyfunction]
fn branch_policy_evaluate_integration_push_native(
    branch: &str,
    remote_old: &str,
    remote_new: &str,
    fast_forward: bool,
    missing: Vec<String>,
) -> Vec<String> {
    integration_push_reasons(branch, remote_old, remote_new, fast_forward, &missing)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(
        branch_policy_validate_name_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(branch_policy_suggest_name_native, module)?)?;
    module.add_function(wrap_pyfunction!(
        branch_policy_force_mode_from_tokens_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        branch_policy_stamp_age_hours_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        branch_policy_validate_bindings_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        branch_policy_second_branch_conflict_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        branch_policy_evaluate_feature_push_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        branch_policy_evaluate_integration_push_native,
        module
    )?)?;
    Ok(())
}

// --------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: &str = "20260905";

    #[test]
    fn validates_valid_shape() {
        let (raw, agent, topic, date) =
            validate_name("claude/branch-policy-20260829", TODAY).unwrap();
        assert_eq!(
            (raw.as_str(), agent.as_str(), topic.as_str(), date.as_str()),
            (
                "claude/branch-policy-20260829",
                "claude",
                "branch-policy",
                "20260829"
            )
        );
    }

    #[test]
    fn refuses_each_class_with_its_message() {
        for (name, needle) in [
            ("no-slash-here-20260829", "no '<agent>/' segment"),
            ("Claude/topic-20260829", "invalid agent slug"),
            ("9agent/topic-20260829", "invalid agent slug"),
            ("claude/nodatehere", "no '<topic>-<yyyymmdd>' segment"),
            ("claude/-topic-20260829", "no '<topic>-<yyyymmdd>' segment"),
            ("claude/topic-2026082", "no '<topic>-<yyyymmdd>' segment"),
            ("claude/topic-20260229", "invalid calendar date"),
        ] {
            let message = validate_name(name, TODAY).unwrap_err();
            assert!(message.contains(needle), "{name}: {message}");
            assert!(message.contains("try '"), "{name}: {message}");
        }
    }

    #[test]
    fn suggestions_are_themselves_valid() {
        for bad in [
            "nodate",
            "Claude/topic-20260829",
            "claude/topic-20260229",
            "bad name here",
            "9team/topic-20260829",
        ] {
            let suggestion = suggest_branch_name(bad, TODAY);
            validate_name(&suggestion, TODAY)
                .unwrap_or_else(|e| panic!("{bad} -> {suggestion}: {e}"));
        }
        assert!(suggest_branch_name("9team/topic-20260829", TODAY).starts_with("a9team/"));
    }

    #[test]
    fn repr_matches_python_quote_rules() {
        assert_eq!(python_repr("claude/x"), "'claude/x'");
        assert_eq!(python_repr("it's"), "\"it's\"");
        assert_eq!(python_repr("say \"hi\""), "'say \"hi\"'");
        assert_eq!(python_repr("both'\""), "'both\\'\"'");
        assert_eq!(python_repr("a\nb"), "'a\\nb'");
    }

    #[test]
    fn slugify_matches_python() {
        assert_eq!(slugify("Claude_X"), "claude-x");
        assert_eq!(slugify("  --a--b--  "), "a-b");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify("a--b"), "a-b");
    }

    #[test]
    fn calendar_boundaries() {
        for (date, ok) in [
            ("20260228", true),
            ("20260229", false),
            ("20240229", true),
            ("19000229", false),
            ("20000229", true),
            ("00000101", false),
            ("20261231", true),
            ("20261301", false),
            ("2026110", false),
        ] {
            assert_eq!(valid_calendar_date(date), ok, "{date}");
        }
    }

    #[test]
    fn force_mode_classification() {
        let push = |extra: &str| -> Vec<String> {
            ["git", "push", extra]
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        };
        assert_eq!(
            force_mode_from_tokens(&["git".into(), "status".into()]),
            "unknown"
        );
        assert_eq!(
            force_mode_from_tokens(&["git".into(), "push".into(), "origin".into(), "main".into()]),
            "none"
        );
        assert_eq!(force_mode_from_tokens(&push("--force-with-lease")), "lease");
        assert_eq!(
            force_mode_from_tokens(&push("--force-with-lease=refs/heads/x:abc")),
            "lease"
        );
        assert_eq!(force_mode_from_tokens(&push("--force")), "bare");
        assert_eq!(
            force_mode_from_tokens(&["git".into(), "push".into(), "-f".into()]),
            "bare"
        );
        assert_eq!(
            force_mode_from_tokens(&[
                "git".into(),
                "push".into(),
                "-f".into(),
                "--force-with-lease".into()
            ]),
            "lease"
        );
    }

    #[test]
    fn stamps_parse_like_fromisoformat() {
        let (us, aware) = parse_stamp("2026-08-29T12:00:00+00:00").unwrap();
        assert!(aware);
        assert_eq!(us, 1_788_004_800 * 1_000_000);
        let (us_z, _) = parse_stamp("2026-08-29T12:00:00Z").unwrap();
        assert_eq!(us_z, us);
        let (us_off, _) = parse_stamp("2026-08-29T12:00:00+05:30").unwrap();
        assert_eq!(us_off, us - 5 * 3600 * 1_000_000 - 30 * 60 * 1_000_000);
        let (us_frac, _) = parse_stamp("2026-08-29T12:00:00.123456+00:00").unwrap();
        assert_eq!(us_frac, us + 123_456);
        let (us_naive, aware) = parse_stamp("2026-08-29T12:00:00").unwrap();
        assert!(!aware);
        assert_eq!(us_naive, us);
        let (us_date_only, aware) = parse_stamp("2026-08-29").unwrap();
        assert!(!aware);
        assert_eq!(us_date_only, 1_787_961_600 * 1_000_000);
        assert!(parse_stamp("not-a-stamp").is_err());
        assert!(parse_stamp("2026-13-01T00:00:00+00:00").is_err());
        assert!(parse_stamp("2026-08-29T25:00:00+00:00").is_err());
    }

    #[test]
    fn store_validation_errors() {
        let row = r#"{"branch":"claude/t-20260829","claim_id":"c1","owner":"claude","created_at":"2026-01-01T00:00:00+00:00","last_push_at":null,"pr_number":null}"#;
        let ok = format!(r#"{{"schema_version":1,"bindings":[{row}]}}"#);
        assert_eq!(validate_store(&ok).unwrap().len(), 1);
        assert!(validate_store(r#"{"bindings":[]}"#)
            .unwrap_err()
            .contains("invalid top-level schema"));
        assert!(validate_store(r#"{"schema_version":2,"bindings":[]}"#)
            .unwrap_err()
            .contains("schema version or bindings are invalid"));
        let dup = format!(r#"{{"schema_version":1,"bindings":[{row},{row}]}}"#);
        assert!(validate_store(&dup)
            .unwrap_err()
            .contains("duplicate branches"));
        assert!(validate_store(r#"{"schema_version":1,"bindings":[123]}"#)
            .unwrap_err()
            .contains("invalid schema"));
        assert!(
            validate_store(r#"{"schema_version":1,"bindings":[{"branch":"x"}]}"#)
                .unwrap_err()
                .contains("invalid schema")
        );
        let float_pr = row.replace("\"pr_number\":null", "\"pr_number\":1.5");
        let store = format!(r#"{{"schema_version":1,"bindings":[{float_pr}]}}"#);
        assert!(validate_store(&store)
            .unwrap_err()
            .contains("pr_number must be int or null"));
        // JSON true passes Python's isinstance(pr_number, int); stored as 1.
        let bool_pr = row.replace("\"pr_number\":null", "\"pr_number\":true");
        let store = format!(r#"{{"schema_version":1,"bindings":[{bool_pr}]}}"#);
        assert_eq!(validate_store(&store).unwrap()[0].pr_number, Some(1));
    }

    #[test]
    fn conflict_requires_a_live_other_branch() {
        let row = |branch: &str| Binding {
            branch: branch.to_owned(),
            claim_id: "c1".to_owned(),
            owner: "claude".to_owned(),
            created_at: "2026-01-01T00:00:00+00:00".to_owned(),
            last_push_at: None,
            pr_number: None,
        };
        let bindings = vec![row("claude/a-20260829"), row("claude/b-20260829")];
        let live = vec!["claude/a-20260829".to_owned()];
        let message = second_branch_conflict(&live, &bindings, "claude/b-20260829", "c1");
        assert!(message.unwrap().contains("already bound to live branch"));
        let dead: Vec<String> = vec![];
        assert!(second_branch_conflict(&dead, &bindings, "claude/b-20260829", "c1").is_none());
    }

    #[test]
    fn feature_reasons_cover_each_refusal() {
        let empty_live: Vec<String> = vec![];
        let bare =
            feature_push_reasons("claude/t-20260829", false, "bare", TODAY, None, &empty_live)
                .unwrap();
        assert!(bare.iter().any(|r| r.contains("bare --force")));
        let none =
            feature_push_reasons("claude/t-20260829", false, "none", TODAY, None, &empty_live)
                .unwrap();
        assert!(none
            .iter()
            .any(|r| r.contains("no force flag was declared")));
        let unknown = feature_push_reasons(
            "claude/t-20260829",
            false,
            "unknown",
            TODAY,
            None,
            &empty_live,
        )
        .unwrap();
        assert!(unknown
            .iter()
            .any(|r| r.contains("could not be established")));
        let bad_name =
            feature_push_reasons("Bad Name", true, "none", TODAY, None, &empty_live).unwrap();
        assert!(bad_name
            .iter()
            .any(|r| r.contains("invalid agent slug") || r.contains("no '<agent>/' segment")));
        let ff = feature_push_reasons("claude/t-20260829", true, "none", TODAY, None, &empty_live)
            .unwrap();
        assert!(ff.is_empty());
    }

    #[test]
    fn integration_reasons_match_python_messages() {
        let reasons = integration_push_reasons("master", "abcdef12", "12345678", false, &[]);
        assert_eq!(
            reasons[0],
            "integration branch 'master' requires a fast-forward push; \
             abcdef12 is not an ancestor of 12345678"
        );
        let reasons = integration_push_reasons("master", "", "1234567890abcdef", false, &[]);
        assert!(reasons[0].contains("(new) is not an ancestor"));
        let missing: Vec<String> = [
            "11111111", "22222222", "33333333", "44444444", "55555555", "66666666", "77777777",
        ]
        .iter()
        .map(|p| format!("{p}00000000000000000000000000000000"))
        .collect();
        let reasons = integration_push_reasons("master", "aaaaaaaa", "bbbbbbbb", true, &missing);
        assert!(reasons[0].contains("would carry 7 commit(s)"));
        assert!(reasons[0].ends_with(": 11111111, 22222222, 33333333, 44444444, 55555555"));
        let reasons = integration_push_reasons("master", "aaaaaaaa", "bbbbbbbb", true, &[]);
        assert!(reasons.is_empty());
    }

    #[test]
    fn age_hours_matches_python_boundaries() {
        let stamp = "2026-08-29T06:00:00+00:00";
        let now = "2026-08-29T12:00:00+00:00";
        assert_eq!(
            branch_policy_stamp_age_hours_native(stamp, now).unwrap(),
            6.0
        );
        let just_over = "2026-08-29T05:59:00+00:00";
        let hours = branch_policy_stamp_age_hours_native(just_over, now).unwrap();
        assert!(hours > 6.0);
    }
}
