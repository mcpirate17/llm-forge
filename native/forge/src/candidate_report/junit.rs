use super::{model::Report, Output};
use crate::candidate_assessment::{CheckResult, CheckStatus};
use anyhow::{ensure, Result};
use std::{collections::BTreeSet, io::Write};

fn escaped(out: &mut Output, text: &str) -> Result<()> {
    for c in text.chars() {
        ensure!(
            matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}'
            | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}'),
            "XML-invalid character in JUnit report"
        );
        let entity = match c {
            '&' => Some("&amp;"),
            '<' => Some("&lt;"),
            '>' => Some("&gt;"),
            '"' => Some("&quot;"),
            '\'' => Some("&apos;"),
            '\t' => Some("&#9;"),
            '\n' => Some("&#10;"),
            '\r' => Some("&#13;"),
            _ => None,
        };
        if let Some(entity) = entity {
            out.write_all(entity.as_bytes())?;
        } else {
            out.write_all(c.encode_utf8(&mut [0; 4]).as_bytes())?;
        }
    }
    Ok(())
}
fn seconds(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}
fn element(out: &mut Output, name: &str, text: &str) -> Result<()> {
    write!(out, "<{name}>")?;
    escaped(out, text)?;
    write!(out, "</{name}>")?;
    Ok(())
}

pub(super) fn render(r: &Report<'_>, out: &mut Output) -> Result<()> {
    let failed = r.checks.iter().filter(|c| is_failed(c)).count();
    let verdict = sentinel(r, failed);
    out.write_all(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuite name=\"candidate-review:",
    )?;
    escaped(out, r.surface)?;
    out.write_all(b":")?;
    escaped(out, r.profile)?;
    write!(
        out,
        "\" tests=\"{}\" failures=\"{}\" skipped=\"{}\" time=\"{}\" id=\"",
        r.checks.len() + usize::from(verdict.is_some()),
        failed + usize::from(verdict.is_some()),
        r.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Skipped)
            .count(),
        seconds(r.duration)
    )?;
    escaped(out, r.id)?;
    out.write_all(b"\"><properties><property name=\"execution_verified\" value=\"false\"/><property name=\"recorded_decision\" value=\"")?;
    escaped(out, r.decision)?;
    out.write_all(b"\"/></properties>")?;
    for check in &r.checks {
        testcase(check, out)?;
    }
    if let Some(message) = verdict {
        out.write_all(b"<testcase name=\"recorded-review-verdict\" classname=\"governance.candidate_review.recorded\" time=\"0.000\"><failure message=\"recorded review evidence\">")?;
        escaped(out, message)?;
        out.write_all(b"</failure></testcase>")?;
    }
    out.write_all(b"</testsuite>\n")?;
    Ok(())
}

fn is_failed(check: &CheckResult) -> bool {
    matches!(check.status, CheckStatus::Failed | CheckStatus::Error)
}

fn sentinel(r: &Report<'_>, failed: usize) -> Option<&'static str> {
    if r.decision == "fail" && failed == 0 {
        return Some(
            "Recorded decision is fail without a failed/error check; execution remains unverified.",
        );
    }
    let represented: BTreeSet<&str> = r
        .checks
        .iter()
        .filter(|c| is_failed(c))
        .map(|c| c.check_id.as_str())
        .collect();
    let top_level = r
        .findings
        .iter()
        .any(|f| r.blocking(f) && !represented.contains(f.check_id.as_str()));
    let per_check = r
        .checks
        .iter()
        .any(|c| !is_failed(c) && c.findings.iter().any(|f| r.blocking(f)));
    if top_level || per_check {
        Some("Recorded blocking evidence is unrepresented by failed/error checks; execution and recorded suppressions remain unverified.")
    } else {
        None
    }
}

fn testcase(check: &CheckResult, out: &mut Output) -> Result<()> {
    out.write_all(b"<testcase name=\"")?;
    escaped(out, &check.check_id)?;
    write!(
        out,
        "\" classname=\"governance.candidate_review\" time=\"{}\">",
        seconds(check.duration_ms)
    )?;
    match check.status {
        CheckStatus::Skipped => {
            out.write_all(b"<skipped message=\"")?;
            escaped(out, check.skipped_reason.as_deref().unwrap_or("skipped"))?;
            out.write_all(b"\"/>")?;
        }
        CheckStatus::Failed | CheckStatus::Error => {
            out.write_all(b"<failure message=\"")?;
            escaped(out, &check.check_id)?;
            out.write_all(b" failed\">")?;
            for (i, f) in check.findings.iter().enumerate() {
                if i != 0 {
                    out.write_all(b"\n")?;
                }
                let severity = serde_json::to_value(f.severity)?;
                escaped(out, severity.as_str().unwrap_or("unknown"))?;
                out.write_all(b" ")?;
                escaped(out, &f.rule_id)?;
                out.write_all(b": ")?;
                escaped(out, &f.message)?;
            }
            out.write_all(b"</failure>")?;
        }
        CheckStatus::Passed | CheckStatus::Cached => {}
    }
    element(out, "system-out", &check.stdout_tail)?;
    element(out, "system-err", &check.stderr_tail)?;
    out.write_all(b"</testcase>")?;
    Ok(())
}
