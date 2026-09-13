//! Port of `tooling/hooks/claude/_post_edit_audit.py`: the PostToolUse
//! Edit/Write body behind `post_edit`. Phase 1 formats the edited file
//! deterministically and silently (`ruff` for Python, `rustfmt` for Rust; a
//! missing or failing formatter is not the hook's failure). Phase 2 reports
//! god files, god functions, commented-out code and bare excepts as advisory
//! `additionalContext`, joined exactly as Python joins them.
//!
//! The structural half of the audit walks a CPython `ast` in Python. Porting
//! the interpreter's parser is out of scope, so the two node rules (god
//! functions, silent fallbacks) are re-derived from source lines: function
//! spans come from an indentation scan (a function runs from its `def` line
//! to the last code line before the first dedent below the `def`'s indent;
//! nested `def`s open their own spans), and an `except` whose body is one
//! `pass`/`continue`/`break` statement is a silent fallback. For ordinary
//! Python -- the only kind this hook has ever judged -- the two agree with
//! `ast`; the corpus (`post_tool_corpus.json`'s `post_edit` cases) pins the
//! messages byte-for-byte. Ordering follows source order, which equals
//! `ast.walk`'s breadth-first order whenever the two rules do not interleave
//! across nesting depths. Known edges, documented rather than hidden: tabs
//! are not expanded (corpus and tree are space-indented), a `def` or `except`
//! at the start of a multi-line string literal can fool the scanner, and
//! `str.splitlines()`'s exotic boundaries (`\v`, `\f`, U+2028) count as one
//! line here.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub const PROSE_SUFFIXES: &[&str] = &[".md", ".txt", ".rst", ".jsonl", ".csv", ".json"];
pub const GOD_FILE_LINES: usize = 1250;
pub const GOD_FUNCTION_LINES: usize = 100;
const FORMATTER_TIMEOUT: Duration = Duration::from_secs(12);

/// `_run_quiet`: run to completion with both outputs dropped, bounded by the
/// 12 s timeout, swallowing any failure (a formatter that cannot run is not
/// this hook's failure). Python's `subprocess.run(timeout=...)` kills on
/// expiry; the poll loop below does the same by hand.
fn run_quiet(argv: &[&str]) {
    let Ok(mut child) = Command::new(argv[0])
        .args(&argv[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
    else {
        return; // FileNotFoundError: the formatter is simply not installed
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {}
        }
        if start.elapsed() >= FORMATTER_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return; // subprocess.TimeoutExpired, swallowed like every failure
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `format_file`: the formatter a suffix selects, silently. `.py` gets both
/// ruff passes; `.rs` gets rustfmt; every other suffix (prose included) is
/// left alone.
fn format_file(path: &str) {
    if path.ends_with(".py") {
        run_quiet(&["ruff", "check", "--fix", "--quiet", path]);
        run_quiet(&["ruff", "format", "--quiet", path]);
    } else if path.ends_with(".rs") {
        run_quiet(&["rustfmt", "--edition", "2021", "--quiet", path]);
    }
}

/// One code line of a Python file: 1-based number, indent width in spaces,
/// and the line with indentation stripped.
struct CodeLine {
    number: usize,
    indent: usize,
    text: String,
}

fn code_lines(content: &str) -> Vec<CodeLine> {
    let mut out = Vec::new();
    for (index, raw) in content.lines().enumerate() {
        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue; // neither blank nor comment-only lines are statements
        }
        out.push(CodeLine {
            number: index + 1,
            indent: raw.len() - trimmed.len(),
            text: trimmed.to_string(),
        });
    }
    out
}

/// A `def` whose span is known: from its header line to the last code line
/// before the first dedent below the header's indent (`ast`'s `lineno` to
/// `end_lineno`).
struct FnScope {
    indent: usize,
    def_line: usize,
    name: String,
    last_code_line: usize,
}

/// A `def` header still open (brackets not yet balanced): continuation lines
/// at any indent belong to the signature, not to a body.
struct OpenHeader {
    indent: usize,
    def_line: usize,
    name: String,
    depth: i32,
}

/// The god-function rule: every `def`/`async def` longer than
/// `GOD_FUNCTION_LINES`, in source order, with `ast`'s own message.
fn god_functions(lines: &[CodeLine]) -> Vec<String> {
    let mut warnings = Vec::new();
    let close = |scope: FnScope, warnings: &mut Vec<String>| {
        let length = scope.last_code_line - scope.def_line + 1;
        if length > GOD_FUNCTION_LINES {
            warnings.push(format!(
                "{}() is {length} lines at line {}. Break it up.",
                scope.name, scope.def_line
            ));
        }
    };
    let mut scopes: Vec<FnScope> = Vec::new();
    let mut headers: Vec<OpenHeader> = Vec::new();
    for line in lines {
        // A code line at or below a scope's `def` indent is outside that
        // function: close it (and the scopes nested in it) here.
        while scopes
            .last()
            .is_some_and(|scope| line.indent <= scope.indent)
        {
            close(scopes.pop().expect("last is some"), &mut warnings);
        }
        // A signature continuation closes its header once balanced; the
        // closing line is inside every enclosing scope either way.
        if let Some(header) = headers.last_mut() {
            header.depth += bracket_delta(&line.text);
            if header.depth <= 0 {
                let header = headers.pop().expect("checked last");
                scopes.push(FnScope {
                    indent: header.indent,
                    def_line: header.def_line,
                    name: header.name,
                    last_code_line: line.number,
                });
            }
            for scope in scopes.iter_mut() {
                scope.last_code_line = line.number;
            }
        } else if let Some(name) = def_header(&line.text) {
            let depth = bracket_delta(&line.text);
            if depth > 0 {
                headers.push(OpenHeader {
                    indent: line.indent,
                    def_line: line.number,
                    name,
                    depth,
                });
            } else {
                scopes.push(FnScope {
                    indent: line.indent,
                    def_line: line.number,
                    name,
                    last_code_line: line.number,
                });
            }
            continue;
        }
        for scope in scopes.iter_mut() {
            scope.last_code_line = line.number;
        }
    }
    for scope in scopes {
        close(scope, &mut warnings);
    }
    warnings
}

/// `(name)` when the line opens a function definition (`async def` included;
/// `def`'s `lineno` is the `def` line itself, decorators excluded).
fn def_header(text: &str) -> Option<String> {
    let after_async = text.strip_prefix("async ").unwrap_or(text);
    let rest = after_async.strip_prefix("def")?;
    if !rest.starts_with(char::is_whitespace) {
        return None; // `defx = ...` or another word starting with "def"
    }
    let name = rest.trim_start();
    let end = name
        .find(|c: char| c == '(' || c.is_whitespace())
        .unwrap_or(name.len());
    if end == 0 {
        return None;
    }
    Some(name[..end].to_string())
}

/// Net open brackets in a line (`def f(a,` leaves one open).
fn bracket_delta(text: &str) -> i32 {
    let mut delta = 0;
    for ch in text.chars() {
        match ch {
            '(' | '[' | '{' => delta += 1,
            ')' | ']' | '}' => delta -= 1,
            _ => {}
        }
    }
    delta
}

/// The silent-fallback rule: an `except` clause whose whole body is one
/// `pass`/`continue`/`break` statement, with `ast`'s own message (the node
/// type's name lowercased is exactly the keyword).
fn silent_fallbacks(lines: &[CodeLine]) -> Vec<String> {
    let mut warnings = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let rest = match line.text.strip_prefix("except") {
            Some(rest)
                if rest.starts_with(':')
                    || rest.starts_with('*')
                    || rest.starts_with(char::is_whitespace) =>
            {
                rest
            }
            _ => continue, // `exception = ...` is not a handler header
        };
        if let Some(keyword) = handler_quiet_keyword(lines, index, rest) {
            warnings.push(format!(
                "Silent fallback at line {}: except block only contains \
                 {keyword}. Log or re-raise.",
                line.number
            ));
        }
    }
    warnings
}

/// `Some(keyword)` when the handler's body is exactly one quiet statement --
/// inline (`except: pass`) or as the sole indented statement.
fn handler_quiet_keyword(
    lines: &[CodeLine],
    index: usize,
    header_rest: &str,
) -> Option<&'static str> {
    if let Some(body) = header_rest.rfind(':').and_then(|colon| {
        let tail = header_rest[colon + 1..].trim();
        (!tail.is_empty()).then_some(tail)
    }) {
        return single_quiet_statement(body);
    }
    let header = &lines[index];
    let first = lines.get(index + 1)?;
    if first.indent <= header.indent {
        return None; // no body: a syntax error in Python, silence here
    }
    let body_indent = first.indent;
    let mut statements = 0usize;
    let mut depth = 0i32;
    for line in &lines[index + 1..] {
        if line.indent <= header.indent {
            break; // the handler body is over
        }
        if line.indent == body_indent && depth == 0 {
            statements += 1;
            let keyword = single_quiet_statement(&line.text);
            if statements > 1 || keyword.is_none() {
                return None; // more than one statement, or a loud one
            }
            depth += bracket_delta(&line.text);
        } else if line.indent >= body_indent {
            depth += bracket_delta(&line.text); // continuation of a statement
        }
    }
    (statements == 1)
        .then(|| single_quiet_statement(&first.text))
        .flatten()
}

/// `Some("pass" | "continue" | "break")` when the statement text is exactly
/// one of the three quiet keywords (a trailing comment is still that
/// statement; anything else is not).
fn single_quiet_statement(text: &str) -> Option<&'static str> {
    let text = text.split('#').next().unwrap_or("").trim_end();
    ["pass", "continue", "break"]
        .into_iter()
        .find(|keyword| text.strip_prefix(*keyword).is_some_and(str::is_empty))
}

/// `audit`: the full rule list for one file, in Python's own order -- god
/// file first, then the structural rules, then the two line counters.
pub fn audit(path: &str, content: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let is_prose = PROSE_SUFFIXES.iter().any(|suffix| path.ends_with(suffix));
    if lines.len() > GOD_FILE_LINES && !is_prose {
        warnings.push(format!("{}: {} lines. Split this file.", path, lines.len()));
    }
    if path.ends_with(".py") {
        let code = code_lines(content);
        warnings.extend(god_functions(&code));
        warnings.extend(silent_fallbacks(&code));
        let commented = lines
            .iter()
            .filter(|line| commented_code_line(line))
            .count();
        if commented > 2 {
            warnings.push(format!(
                "{commented} lines of commented-out code. Delete them."
            ));
        }
        let bare = lines.iter().filter(|line| bare_except_line(line)).count();
        if bare > 0 {
            warnings.push(format!(
                "{bare} bare except clause(s). Catch specific exceptions."
            ));
        }
    }
    warnings
}

/// `COMMENTED_CODE.match`: a commented-out def/class/import/from/return/
/// raise/for/while header.
fn commented_code_line(line: &str) -> bool {
    let rest = line
        .trim_start()
        .strip_prefix('#')
        .unwrap_or("")
        .trim_start();
    [
        "def ", "class ", "import ", "from ", "return ", "raise ", "for ", "while ",
    ]
    .iter()
    .any(|prefix| rest.starts_with(prefix))
}

/// `BARE_EXCEPT.match` (`^\s*except\s*:`): `except:` with no exception type.
fn bare_except_line(line: &str) -> bool {
    line.trim_start()
        .strip_prefix("except")
        .is_some_and(|rest| rest.trim_start().starts_with(':'))
}

fn quiet_post() -> Value {
    json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
}

/// `hook_output`: format then audit the edited file; a file that does not
/// exist (or a payload without its path) is silence. The warnings join into
/// one advisory `additionalContext`, prefixed exactly as Python prefixes it.
pub fn hook_output(payload: &Value) -> anyhow::Result<Value> {
    let tool_input = payload.get("tool_input").filter(|value| value.is_object());
    let path = tool_input
        .and_then(|input| input.get("file_path"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if path.is_empty() || !Path::new(path).is_file() {
        return Ok(quiet_post());
    }
    format_file(path);
    let warnings = match std::fs::read_to_string(path) {
        Ok(content) => audit(path, &content),
        Err(_) => Vec::new(), // OSError/UnicodeDecodeError: nothing to audit
    };
    if warnings.is_empty() {
        return Ok(quiet_post());
    }
    Ok(json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": format!("POST-EDIT AUDIT: {}", warnings.join(" | ")),
        }
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn a_god_function_carries_asts_own_message() {
        let body: String = "    pass\n".repeat(120);
        let content = format!("def giant():\n{body}\ndef after():\n    pass\n");
        assert_eq!(
            audit("/repo/giant.py", &content),
            vec!["giant() is 121 lines at line 1. Break it up.".to_string()]
        );
        // `end_lineno` excludes trailing comment lines inside the span.
        let with_comment = format!("def giant():\n{body}    # tail comment\n");
        assert_eq!(
            audit("/repo/g.py", &with_comment),
            vec!["giant() is 121 lines at line 1. Break it up.".to_string()]
        );
    }

    #[test]
    fn prose_files_skip_the_god_file_rule() {
        let content = "line\n".repeat(1251);
        assert!(audit("/repo/big.md", &content).is_empty());
        assert_eq!(
            audit("/repo/big.toml", &content),
            vec!["/repo/big.toml: 1251 lines. Split this file.".to_string()]
        );
    }

    #[test]
    fn silent_fallbacks_and_the_line_counters_match_python() {
        let content = "try:\n    x = 1\nexcept ValueError:\n    pass\n";
        assert_eq!(
            audit("/repo/a.py", content),
            vec![
                "Silent fallback at line 3: except block only contains pass. \
                  Log or re-raise."
                    .to_string()
            ]
        );
        let commented = "# def old()\n# import sys\n# return 3\nx = 1\n";
        assert_eq!(
            audit("/repo/b.py", commented),
            vec!["3 lines of commented-out code. Delete them.".to_string()]
        );
        let bare = "try:\n    x = 1\nexcept:\n    raise\n";
        assert_eq!(
            audit("/repo/c.py", bare),
            vec!["1 bare except clause(s). Catch specific exceptions.".to_string()]
        );
    }
}
