//! Deterministic presentation-only reductions. Exact bytes live in recovery.
use std::borrow::Cow;

pub struct Projection {
    pub text: String,
    pub errors: usize,
    pub warnings: usize,
    pub omitted_lines: usize,
}

/// Keep compiler diagnostics plus nearby source/caret lines, and a short tail.
/// Unknown log formats are not interpreted as proof of success or failure.
pub fn logs(input: &str) -> Projection {
    let lines: Vec<_> = input.lines().collect();
    let mut keep = vec![false; lines.len()];
    let mut errors = 0;
    let mut warnings = 0;
    for (i, line) in lines.iter().enumerate() {
        let plain = strip_ansi(line);
        let (error, warning) = severity(&plain);
        errors += usize::from(error);
        warnings += usize::from(warning);
        if error || warning {
            keep[i.saturating_sub(2)..(i + 7).min(lines.len())].fill(true);
        }
    }
    keep[lines.len().saturating_sub(12)..].fill(true);
    let omitted_lines = keep.iter().filter(|&&selected| !selected).count();
    if omitted_lines == 0 {
        return Projection {
            text: input.into(),
            errors,
            warnings,
            omitted_lines,
        };
    }
    let mut text = String::new();
    let mut previous = None;
    for (i, _) in keep
        .into_iter()
        .enumerate()
        .filter(|(_, selected)| *selected)
    {
        if previous.is_none_or(|p| i != p + 1) {
            text.push_str("[log lines omitted; exact original in recovery]\n");
        }
        text.push_str(lines[i]);
        text.push('\n');
        previous = Some(i);
    }
    Projection {
        text,
        errors,
        warnings,
        omitted_lines,
    }
}

fn severity(line: &str) -> (bool, bool) {
    // Cargo's JSON diagnostics contain escaped rendered text. Match its structured
    // level, while retaining the complete original line rather than inventing prose.
    if line.starts_with('{') && line.contains("compiler-message") {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            let level = value["message"]["level"].as_str().unwrap_or("");
            return (
                matches!(level, "error" | "failure-note"),
                level == "warning",
            );
        }
    }
    let line = line.trim_start();
    let error = line.starts_with("error:")
        || line.starts_with("error[")
        || line.starts_with("fatal error:")
        || line.starts_with("FAILED:")
        || line.contains(": error:")
        || line.contains(": fatal error:")
        || line.contains(": error C")
        || line.contains("undefined reference to")
        || line.starts_with("ERROR:");
    let warning = line.starts_with("warning:")
        || line.starts_with("warning[")
        || line.contains(": warning:")
        || line.contains(": warning C");
    (error, warning)
}

fn strip_ansi(line: &str) -> Cow<'_, str> {
    if !line.contains('\u{1b}') {
        return Cow::Borrowed(line);
    }
    let mut output = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.clone().next() == Some('[') {
            chars.next();
            for byte in chars.by_ref() {
                if ('@'..='~').contains(&byte) {
                    break;
                }
            }
        } else {
            output.push(c);
        }
    }
    Cow::Owned(output)
}

/// Not an applicable patch: retains headers and every changed line verbatim.
pub fn diff(input: &str) -> Option<String> {
    if !input.lines().any(|line| line.starts_with("@@ ")) {
        return None;
    }
    let mut text = String::from("[diff projection; exact applicable patch in recovery]\n");
    let mut omitted = false;
    for line in input.lines() {
        if line.starts_with(' ') {
            omitted = true;
            continue;
        }
        if omitted {
            text.push_str("[unchanged diff context omitted]\n");
            omitted = false;
        }
        text.push_str(line);
        text.push('\n');
    }
    if omitted {
        text.push_str("[unchanged diff context omitted]\n");
    }
    (text.len() < input.len()).then_some(text)
}
