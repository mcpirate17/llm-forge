//! Strict single-source Mull Elements validation for the confined profile.
//! Callers independently hold report bytes, inputs, tool identities and process cleanup.
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};

const MAX_MUTANTS: usize = 100_000;
const MAX_TEXT: usize = 32 * 1024 * 1024;
const MAX_FIELD: usize = 4096;

fn text<'a>(value: &'a Value, key: &str, empty: bool) -> Result<&'a str, String> {
    let value = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Mull mutant lacks {key}"))?;
    if value.len() > MAX_FIELD
        || (!empty && value.is_empty())
        || (!empty && value.chars().any(char::is_control))
    {
        return Err(format!("invalid Mull mutant {key}"));
    }
    Ok(value)
}

fn lines(source: &str) -> Vec<&str> {
    // Match the legacy row converter's Python-style splitlines, including CRLF.
    let mut result = Vec::new();
    let mut start = 0;
    let mut iter = source.char_indices().peekable();
    while let Some((index, ch)) = iter.next() {
        if matches!(
            ch,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            result.push(&source[start..index]);
            start = index + ch.len_utf8();
            if ch == '\r' && iter.peek().is_some_and(|(_, next)| *next == '\n') {
                let (next, _) = iter.next().expect("peeked newline");
                start = next + 1;
            }
        }
    }
    if start < source.len() {
        result.push(&source[start..]);
    }
    result
}

fn position(value: &Value, source: &[&str]) -> Result<(usize, usize), String> {
    let line = value
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or("invalid Mull line")?;
    let col = value
        .get("column")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or("invalid Mull column")?;
    let text = source.get(line - 1).ok_or("Mull line outside source")?;
    let prefix = text
        .as_bytes()
        .get(..col - 1)
        .ok_or("Mull column outside source")?;
    if !prefix.is_ascii() {
        return Err("Mull byte column cannot be safely mapped to characters".into());
    }
    Ok((line, col))
}

fn validate_mutant(
    mutant: &Value,
    source: &[&str],
    ids: &mut BTreeSet<String>,
) -> Result<bool, String> {
    let id = text(mutant, "id", false)?;
    if !ids.insert(id.to_owned()) {
        return Err("duplicate Mull mutant id".into());
    }
    text(mutant, "mutatorName", false)?;
    text(mutant, "replacement", true)?;
    let status = text(mutant, "status", false)?;
    if !matches!(
        status,
        "Killed"
            | "Survived"
            | "NoCoverage"
            | "Timeout"
            | "RuntimeError"
            | "CompileError"
            | "Ignored"
    ) {
        return Err("unknown Mull mutant status".into());
    }
    let location = mutant
        .get("location")
        .ok_or("Mull mutant has no location")?;
    let start = position(
        location.get("start").ok_or("Mull location has no start")?,
        source,
    )?;
    let end = position(
        location.get("end").ok_or("Mull location has no end")?,
        source,
    )?;
    if end < start {
        return Err("Mull location is reversed".into());
    }
    Ok(matches!(status, "Killed" | "Survived"))
}

/// Parse one complete report for one selected source. This is not execution attestation.
pub fn rows(
    report: &Value,
    worktree: &Path,
    source_path: &Path,
    source_bytes: &str,
) -> Result<Vec<Value>, String> {
    if !worktree.is_absolute() || !source_path.is_absolute() || source_bytes.len() > MAX_TEXT {
        return Err("invalid confined Mull source or size".into());
    }
    let relative = source_path
        .strip_prefix(worktree)
        .map_err(|_| "Mull source outside worktree")?;
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err("invalid Mull source path".into());
    }
    let name = source_path
        .to_str()
        .ok_or("Mull source path is not UTF-8")?;
    let files = report
        .get("files")
        .and_then(Value::as_object)
        .ok_or("Mull report files missing")?;
    if files.len() != 1 {
        return Err("Mull report must contain exactly one source".into());
    }
    let entry = files
        .get(name)
        .ok_or("Mull report source is not selected source")?;
    if entry.get("source").and_then(Value::as_str) != Some(source_bytes) {
        return Err("Mull report source differs from held input".into());
    }
    let mutants = entry
        .get("mutants")
        .and_then(Value::as_array)
        .ok_or("Mull report mutants missing")?;
    if mutants.is_empty() || mutants.len() > MAX_MUTANTS {
        return Err("invalid Mull mutant count".into());
    }
    let source = lines(source_bytes);
    let mut ids = BTreeSet::new();
    let mut scored = false;
    for mutant in mutants {
        scored |= validate_mutant(mutant, &source, &mut ids)?;
    }
    if !scored {
        return Err("Mull report has no scored covered mutants".into());
    }
    normalize(mutants, relative, &source)
}

fn source_slice(source: &[&str], start: (usize, usize), end: (usize, usize)) -> String {
    let (first, from) = (start.0 - 1, start.1 - 1);
    let (last, to) = (end.0 - 1, end.1 - 1);
    if first == last {
        return source[first][from..to].to_owned();
    }
    let mut pieces = vec![&source[first][from..]];
    pieces.extend_from_slice(&source[first + 1..last]);
    pieces.push(&source[last][..to]);
    pieces.join("\n")
}

fn normalize(mutants: &[Value], relative: &Path, source: &[&str]) -> Result<Vec<Value>, String> {
    let path = relative.to_str().ok_or("Mull relative path is not UTF-8")?;
    crate::mutation_snapshot_inputs::validate_path(path)?;
    let mut entries = Vec::with_capacity(mutants.len());
    for mutant in mutants {
        let location = &mutant["location"];
        let start = position(&location["start"], source)?;
        let end = position(&location["end"], source)?;
        let operator = text(mutant, "mutatorName", false)?.to_owned();
        let replacement = text(mutant, "replacement", true)?.to_owned();
        let original = source_slice(source, start, end);
        let outcome = match text(mutant, "status", false)? {
            "Killed" => "KILLED",
            "Survived" => "SURVIVED",
            "NoCoverage" => "NO_COVERAGE",
            "Timeout" => "TIMED_OUT",
            "RuntimeError" => "ERROR",
            "CompileError" | "Ignored" => "UNVIABLE",
            _ => return Err("unknown Mull mutant status".into()),
        };
        entries.push((start, operator, original, replacement, outcome));
    }
    entries.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
    let names: Vec<[&str; 4]> = entries
        .iter()
        .map(|(_, op, original, changed, _)| {
            [path, op.as_str(), original.as_str(), changed.as_str()]
        })
        .collect();
    let ids = crate::mutation_generated_identity::identify(&names);
    Ok(entries.iter().zip(ids).map(|((location, operator, original, changed, outcome), id)|
        serde_json::json!({"id":id,"outcome":outcome,"path":path,"line":location.0,"operator":operator,"original_text":original,"mutated_text":changed})).collect())
}

#[cfg(test)]
#[path = "mutation_mull_report_tests.rs"]
mod tests;
