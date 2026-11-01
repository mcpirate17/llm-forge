use super::{
    model::{Report, WAIVED},
    Output,
};
use anyhow::Result;
use serde_json::{json, Value};
use std::{collections::BTreeSet, io::Write};

pub(super) fn render(r: &Report<'_>, out: &mut Output) -> Result<()> {
    out.write_all(br#"{"$schema":"https://json.schemastore.org/sarif-2.1.0.json","version":"2.1.0","runs":[{"tool":{"driver":{"name":"LLM Candidate Review","rules":["#)?;
    let mut seen = BTreeSet::new();
    let mut first = true;
    for f in &r.findings {
        let id = format!("{}/{}", f.check_id, f.rule_id);
        if seen.insert(id.clone()) {
            comma(out, &mut first)?;
            super::json(
                out,
                &json!({"id":id,"shortDescription":{"text":f.rule_id},
                "help":{"text":f.help.as_deref().filter(|s| !s.is_empty()).unwrap_or(&f.message)}}),
            )?;
        }
    }
    out.write_all(br#"]}},"automationDetails":{"id":"#)?;
    super::json(out, &r.id)?;
    out.write_all(br#"},"properties":{"executionVerified":false,"recordedDecision":"#)?;
    super::json(out, &r.decision)?;
    out.write_all(br#"},"results":["#)?;
    first = true;
    for f in &r.findings {
        comma(out, &mut first)?;
        super::json(out, &result(r, f)?)?;
    }
    out.write_all(b"]}]}\n")?;
    Ok(())
}
fn comma(out: &mut Output, first: &mut bool) -> Result<()> {
    if !*first {
        out.write_all(b",")?;
    }
    *first = false;
    Ok(())
}
fn result(r: &Report<'_>, f: &crate::candidate_assessment::Finding) -> Result<Value> {
    use crate::candidate_assessment::Severity;
    let level = match f.severity {
        Severity::Critical | Severity::High => "error",
        Severity::Medium => "warning",
        _ => "note",
    };
    let mut value = json!({"ruleId":format!("{}/{}", f.check_id, f.rule_id),"level":level,
        "message":{"text":f.message},"partialFingerprints":{"governanceFingerprint":f.fingerprint},
        "properties":{"recordedExceptionId":f.exception_id,"candidateTree":r.tree,
            "recordedInherited":f.inherited,"recordedWaived":f.rule_id == WAIVED,
            "recordedBlocking":r.blocking(f),"executionVerified":false}});
    if let Some(column) = f.column {
        value["properties"]["recordedColumn"] = json!(column);
    }
    if let Some(path) = f.path.as_ref().filter(|p| !p.is_empty()) {
        let mut location = json!({"physicalLocation":{"artifactLocation":{"uri":uri(path)?}}});
        let mut region = serde_json::Map::new();
        if let Some(line) = f.line {
            region.insert("startLine".into(), json!(line));
        }
        if !region.is_empty() {
            location["physicalLocation"]["region"] = Value::Object(region);
        }
        value["locations"] = json!([location]);
    }
    Ok(value)
}

fn uri(path: &str) -> Result<String> {
    let mut out = Output::default();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.write_all(&[byte])?;
        } else {
            write!(out, "%{byte:02X}")?;
        }
    }
    Ok(String::from_utf8(out.bytes).expect("URI output is ASCII"))
}
