use super::{
    model::{Report, WAIVED},
    Output,
};
use anyhow::Result;
use std::{fmt, io::Write};

/// Terminal controls, bidi overrides and line separators cannot create forged report lines.
struct Safe<'a>(&'a str);
impl fmt::Display for Safe<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        for ch in self.0.chars() {
            if ch.is_control()
                || matches!(ch, '\u{061c}' | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
            {
                write!(out, "\\u{{{:x}}}", u32::from(ch))?;
            } else {
                write!(out, "{ch}")?;
            }
        }
        Ok(())
    }
}

pub(super) fn render(r: &Report<'_>, out: &mut Output) -> Result<()> {
    writeln!(
        out,
        "candidate-review recorded decision={} | {}/{} | tree {} | {} ms | {} cache hits",
        Safe(r.decision),
        Safe(r.surface),
        Safe(r.profile),
        Safe(&r.tree.chars().take(12).collect::<String>()),
        r.duration,
        r.hits
    )?;
    writeln!(
        out,
        "execution=unverified; receipt rendering is not approval"
    )?;
    let waived = r.findings.iter().filter(|f| f.rule_id == WAIVED).count();
    let excepted = r
        .findings
        .iter()
        .filter(|f| f.rule_id != WAIVED && f.exception_id.as_ref().is_some_and(|id| !id.is_empty()))
        .count();
    let inherited = r
        .findings
        .iter()
        .filter(|f| {
            f.rule_id != WAIVED
                && !f.exception_id.as_ref().is_some_and(|id| !id.is_empty())
                && f.inherited
        })
        .count();
    let blocking = r.findings.iter().filter(|f| r.blocking(f)).count();
    let advisory = r.findings.len() - waived - excepted - inherited - blocking;
    writeln!(out, "blocking={blocking} advisory={advisory} waived={waived} excepted={excepted} inherited={inherited} receipt={}", Safe(r.id))?;
    let mut shown = 0usize;
    for finding in r.findings.iter().filter(|f| f.rule_id == WAIVED).chain(
        r.findings
            .iter()
            .filter(|f| r.blocking(f))
            .chain(
                r.findings
                    .iter()
                    .filter(|f| f.rule_id != WAIVED && !r.blocking(f)),
            )
            .take(20),
    ) {
        let severity = serde_json::to_value(finding.severity)?;
        write!(
            out,
            "- {} {}/{} {}",
            Safe(severity.as_str().unwrap_or("unknown")),
            Safe(&finding.check_id),
            Safe(&finding.rule_id),
            Safe(finding.path.as_deref().unwrap_or("governance"))
        )?;
        if let Some(line) = finding.line {
            write!(out, ":{line}")?;
        }
        write!(out, ": {}", Safe(&finding.message))?;
        if finding.rule_id == WAIVED {
            write!(out, " [waived]")?;
        }
        if let Some(id) = finding.exception_id.as_ref().filter(|id| !id.is_empty()) {
            write!(out, " [excepted:{}]", Safe(id))?;
        }
        if finding.inherited {
            write!(out, " [inherited]")?;
        }
        writeln!(out)?;
        shown += 1;
    }
    if shown < r.findings.len() {
        writeln!(
            out,
            "- ... {} more findings in the JSON receipt",
            r.findings.len() - shown
        )?;
    }
    policy_notes(r, out)
}

fn policy_notes(r: &Report<'_>, out: &mut Output) -> Result<()> {
    for (key, label) in [
        ("expired_exceptions", "EXPIRED exception"),
        ("expired_mutation_waivers", "EXPIRED mutation waiver"),
        ("unmatched_exceptions", "STALE exception"),
    ] {
        if let Some(notes) = r.payload["policy"].get(key) {
            let notes = notes
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("policy report notes must be arrays"))?;
            for note in notes {
                let get = |key| {
                    note.get(key)
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| anyhow::anyhow!("policy report note missing text field"))
                };
                writeln!(
                    out,
                    "- {label} {} ({}, expires {}): {}",
                    Safe(get("id")?),
                    Safe(get("owner")?),
                    Safe(get("expires")?),
                    Safe(get("path")?)
                )?;
            }
        }
    }
    Ok(())
}
