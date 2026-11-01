//! Pure supervisor-report mapping. Findings are execution observations, never
//! approval/completeness/authentication. Ordinary fingerprints are finalized by
//! the existing candidate assessor; jscpd has its legacy aggregate override.
use crate::candidate_assessment::{CheckResult, CheckStatus, Finding, Severity};
use crate::confined_execution::{Outcome, Report};
use crate::process_capture_tail::MAX_OUTPUT_CHARS;
use anyhow::{ensure, Context, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionEvidence {
    Available(String),
    Unavailable(String),
}
struct Check {
    id: String,
    severity: Severity,
    timeout: u64,
    wall_timeout: u64,
    memory: u64,
    maximum: usize,
}
fn check(value: &Value) -> Result<Check> {
    ensure!(
        value["kind"].as_str() == Some("command"),
        "result mapping requires a normalized command check"
    );
    let id = value["check_id"]
        .as_str()
        .context("normalized check ID must be a string")?;
    ensure!(
        !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control),
        "invalid normalized check ID"
    );
    let severity = serde_json::from_value::<Severity>(value["severity"].clone())
        .context("invalid normalized severity")?;
    let timeout = number(value, "timeout_seconds", 3600)?;
    let wall = value["wall_timeout_override"]
        .as_u64()
        .context("invalid normalized wall timeout")?;
    ensure!(wall <= 86400, "normalized wall timeout exceeds one day");
    let memory = number(value, "memory_mb", 65536)?;
    let maximum = usize::try_from(number(value, "max_output_chars", MAX_OUTPUT_CHARS as u64)?)?;
    Ok(Check {
        id: id.into(),
        severity,
        timeout,
        wall_timeout: if wall == 0 { timeout } else { wall },
        memory,
        maximum,
    })
}
fn number(value: &Value, field: &str, maximum: u64) -> Result<u64> {
    let number = value[field]
        .as_u64()
        .with_context(|| format!("normalized {field} must be an integer"))?;
    ensure!(
        (1..=maximum).contains(&number),
        "normalized {field} exceeds admission range"
    );
    Ok(number)
}
fn whitespace(character: char) -> bool {
    character.is_whitespace() || matches!(character, '\u{001c}'..='\u{001f}')
}
fn trim(value: &str) -> &str {
    value.trim_matches(whitespace)
}
fn line_break(character: char) -> bool {
    matches!(
        character,
        '\n' | '\r'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{001c}'
            | '\u{001d}'
            | '\u{001e}'
            | '\u{0085}'
            | '\u{2028}'
            | '\u{2029}'
    )
}
fn report_bounds(report: &Report, maximum: usize) -> Result<()> {
    ensure!(
        (1..=MAX_OUTPUT_CHARS).contains(&maximum),
        "capture character limit must be 1..262144"
    );
    ensure!(
        report.stdout.chars().count() <= maximum && report.stderr.chars().count() <= maximum,
        "supervisor tail exceeds capture character limit"
    );
    ensure!(
        report.errors.len() <= 64,
        "supervisor error count exceeds 64"
    );
    let mut bytes = 0_usize;
    for error in &report.errors {
        ensure!(error.len() <= 4096, "supervisor error exceeds byte limit");
        bytes = bytes
            .checked_add(error.len())
            .context("supervisor error size overflow")?;
        ensure!(bytes <= 64 * 1024, "supervisor errors exceed byte limit");
    }
    ensure!(
        report.resources.path.as_os_str().len() <= 4096,
        "resource evidence path exceeds byte limit"
    );
    Ok(())
}
fn complete_tail(value: &str, produced: u64, maximum: usize) -> bool {
    let count = value.chars().count();
    count < maximum
        || (count == maximum && produced == value.len() as u64 && !value.contains('\u{fffd}'))
}
/// capture_chars MUST equal the actual per-stream TailBuffer limit used by the
/// version probe. Below-limit tails are complete. At the limit, replacement or
/// newline byte ambiguity is rejected unless byte equality proves no lost prefix.
pub fn version(report: &Report, capture_chars: usize) -> Result<VersionEvidence> {
    report_bounds(report, capture_chars)?;
    let unavailable = |message: &str| Ok(VersionEvidence::Unavailable(message.into()));
    if !report.cleanup_complete {
        return unavailable("version process cleanup incomplete");
    }
    if !report.resources.verified {
        return unavailable("version process containment unverified");
    }
    if !report.errors.is_empty() {
        return unavailable("version process infrastructure failed");
    }
    if report.outcome != Outcome::Exited || report.exit_code != Some(0) || report.signal.is_some() {
        return unavailable("version command failed or did not exit successfully");
    }
    // Stdout preference is only meaningful if its prefix is intact; even a
    // whitespace tail could have discarded a nonempty stdout version line.
    if !complete_tail(&report.stdout, report.stdout_bytes, capture_chars) {
        return unavailable("version stdout capture may have discarded the first line");
    }
    let stdout = trim(&report.stdout);
    let selected = if !stdout.is_empty() {
        stdout
    } else {
        if !complete_tail(&report.stderr, report.stderr_bytes, capture_chars) {
            return unavailable("version stderr capture may have discarded the first line");
        }
        trim(&report.stderr)
    };
    let first = selected.split(line_break).next().unwrap_or("");
    Ok(VersionEvidence::Available(if first.is_empty() {
        "unknown".into()
    } else {
        first.chars().take(500).collect()
    }))
}
fn normalized_files(files: &[String]) -> Result<Vec<String>> {
    ensure!(files.len() <= 100_000, "result file count exceeds 100000");
    let mut bytes = 0_usize;
    let mut result = BTreeSet::new();
    for file in files {
        crate::candidate_source::safe_path(file.as_bytes())?;
        ensure!(
            !file.contains(['\n', '\r']),
            "result file path contains newline"
        );
        bytes = bytes
            .checked_add(file.len())
            .context("result file bytes overflow")?;
        ensure!(bytes <= 8 * 1024 * 1024, "result files exceed byte limit");
        result.insert(file.clone());
    }
    Ok(result.into_iter().collect())
}
fn command_bounds(command: &[String]) -> Result<()> {
    ensure!(command.len() <= 4096, "result command count exceeds 4096");
    let mut bytes = 0_usize;
    for argument in command {
        ensure!(
            argument.len() <= 131_071 && !argument.contains('\0'),
            "invalid result command argument"
        );
        bytes = bytes
            .checked_add(argument.len())
            .and_then(|n| n.checked_add(1))
            .context("result argv bytes overflow")?;
        ensure!(bytes <= 1024 * 1024, "result command exceeds byte limit");
    }
    Ok(())
}
fn exit_code(report: &Report) -> Option<i64> {
    report
        .signal
        .map(|signal| -i64::from(signal))
        .or_else(|| report.exit_code.map(i64::from))
}
fn finding(
    check: &Check,
    rule: &str,
    severity: Severity,
    message: String,
    code: Option<i64>,
) -> Finding {
    let mut evidence = Map::new();
    if let Some(code) = code {
        evidence.insert("exit_code".into(), json!(code));
    }
    Finding {
        check_id: check.id.clone(),
        rule_id: rule.into(),
        severity,
        message,
        path: None,
        line: None,
        column: None,
        help: None,
        evidence,
        fingerprint: String::new(),
        exception_id: None,
        inherited: false,
    }
}
fn combined(report: &Report) -> Result<String> {
    let stdout = trim(&report.stdout);
    let stderr = trim(&report.stderr);
    let bytes = stdout
        .len()
        .checked_add(stderr.len())
        .and_then(|n| n.checked_add(usize::from(!stdout.is_empty() && !stderr.is_empty())))
        .context("analyzer message bytes overflow")?;
    ensure!(
        bytes <= MAX_MESSAGE_BYTES,
        "analyzer finding message exceeds 1 MiB model byte limit"
    );
    Ok(match (stdout.is_empty(), stderr.is_empty()) {
        (true, _) => stderr.into(),
        (_, true) => stdout.into(),
        _ => format!("{stdout}\n{stderr}"),
    })
}
fn collapse(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut space = false;
    for character in value.chars() {
        if whitespace(character) {
            space = !out.is_empty();
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(character);
        }
    }
    out
}
fn jscpd_fingerprint(output: &str) -> String {
    let mut lines = output.split(line_break);
    let Some(marker) = lines.find(|line| {
        line.starts_with("ERROR: jscpd found ")
            && line.ends_with("CAUSED by this candidate's changed files:")
    }) else {
        return String::new();
    };
    let marker = collapse(marker);
    let mut pairs: Vec<_> = lines
        .filter(|line| line.contains(" <-> ") && trim(line).ends_with(" lines)"))
        .map(collapse)
        .collect();
    if pairs.is_empty() {
        return String::new();
    }
    pairs.sort();
    let value =
        json!({"check_id":"jscpd","rule_id":"analyzer-finding","marker":marker,"result_set":pairs});
    let canonical = crate::json_canon::canonical_json(&value);
    format!("{:x}", Sha256::digest(canonical.as_bytes()))[..24].into()
}
fn observation(check: &Check, report: &Report) -> Result<Option<Finding>> {
    let code = exit_code(report);
    let critical = |message: &str| {
        finding(
            check,
            "analyzer-crash",
            Severity::Critical,
            message.into(),
            code,
        )
    };
    if !report.cleanup_complete {
        return Ok(Some(critical("required analyzer cleanup incomplete")));
    }
    if !report.resources.verified {
        return Ok(Some(critical("required analyzer containment unverified")));
    }
    if !report.errors.is_empty() {
        return Ok(Some(critical(
            "required analyzer supervision or cleanup failed",
        )));
    }
    match report.outcome {
        Outcome::TimedOut => {
            return Ok(Some(finding(
                check,
                "analyzer-timeout",
                Severity::Critical,
                format!(
                    "required analyzer exceeded {}s wall time",
                    check.wall_timeout
                ),
                code,
            )));
        }
        Outcome::Cancelled => return Ok(Some(critical("required analyzer cancelled"))),
        Outcome::Failed => {
            return Ok(Some(critical(
                "required analyzer could not execute or supervision failed",
            )));
        }
        Outcome::Exited => {}
    }
    let Some(code) = code else {
        return Ok(Some(critical("required analyzer terminal state missing")));
    };
    if report.signal.is_some() || code < 0 || matches!(code, 126 | 127) {
        let output = combined(report)?;
        return Ok(Some(finding(
            check,
            "analyzer-crash",
            Severity::Critical,
            if output.is_empty() {
                format!("analyzer exited {code}")
            } else {
                output
            },
            Some(code),
        )));
    }
    if code == 0 {
        return Ok(None);
    }
    let output = combined(report)?;
    let mut result = finding(
        check,
        "analyzer-finding",
        check.severity,
        if output.is_empty() {
            format!("analyzer exited {code}")
        } else {
            output
        },
        Some(code),
    );
    if check.id == "jscpd" {
        result.fingerprint = jscpd_fingerprint(&result.message);
    }
    Ok(Some(result))
}
/// Invalid normalized/admission data is an error, never a synthesized pass.
/// Report describes the analyzer attempt; version-probe evidence is separate.
pub fn map_result(
    normalized: &Value,
    files: &[String],
    command: &[String],
    version: Option<&VersionEvidence>,
    report: &Report,
) -> Result<CheckResult> {
    let check = check(normalized)?;
    report_bounds(report, check.maximum)?;
    command_bounds(command)?;
    let files = normalized_files(files)?;
    let (tool_version, version_error) = match version {
        Some(VersionEvidence::Available(value)) => {
            ensure!(
                !trim(value).is_empty()
                    && value.chars().count() <= 500
                    && !value.chars().any(line_break),
                "invalid available version evidence"
            );
            (Some(value.clone()), None)
        }
        Some(VersionEvidence::Unavailable(error)) => {
            ensure!(
                !trim(error).is_empty() && error.len() <= 4096,
                "invalid version error evidence"
            );
            (None, Some(error.as_str()))
        }
        None => (None, Some("no version evidence")),
    };
    let mut findings = Vec::new();
    if let Some(error) = version_error {
        let mut unavailable = finding(
            &check,
            "required-analyzer-unavailable",
            Severity::Critical,
            format!("required analyzer is unavailable: {error}"),
            None,
        );
        unavailable.help = Some(
            "Install the pinned analyzer; required checks never skip on missing tools.".into(),
        );
        findings.push(unavailable);
    } else if let Some(finding) = observation(&check, report)? {
        findings.push(finding);
    }
    let metrics = serde_json::from_value(
        json!({"timeout_seconds":check.timeout,"wall_timeout_seconds":check.wall_timeout,"memory_limit_mb":check.memory,
        "outcome":report.outcome,"signal":report.signal,"stdout_bytes":report.stdout_bytes,
        "stderr_bytes":report.stderr_bytes,"cleanup_complete":report.cleanup_complete,"supervisor_errors":report.errors,
        "resources":report.resources,"cpu_seconds":report.cpu_seconds,"address_space_bytes":report.address_space_bytes}),
    )?;
    Ok(CheckResult {
        check_id: check.id,
        status: if findings.is_empty() {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
        duration_ms: report.duration_ms,
        findings,
        files,
        command: command.to_vec(),
        tool_version,
        cache_key: None,
        cache_hit: false,
        exit_code: exit_code(report),
        skipped_reason: None,
        stdout_tail: report.stdout.clone(),
        stderr_tail: report.stderr.clone(),
        metrics,
    })
}

#[cfg(test)]
mod tests;
