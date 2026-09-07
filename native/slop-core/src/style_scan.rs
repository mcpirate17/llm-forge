//! Comment and dead-pattern detectors for Python sources.
//!
//! Two laws in CLAUDE.md had no detector at all. "No effort-signaling code" is
//! unenforced because no linter reads comments -- ruff discards them. "Fail
//! loud, no dead scaffolding" is unenforced for stubs and pass-through wrappers
//! because ruff's dead-code rules stop at unused *names*.
//!
//! Rust owns the parse, the walk and the string work; the Python caller owns
//! argv and printing.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use tree_sitter::Node;

use crate::engine::parse;

/// A line-addressed rule violation, ordered the way the gate prints it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    pub rule: &'static str,
    pub message: String,
}

/// Escape hatches, spelled like the ones detector_scan.rs already honours.
const ALLOW_STUB: &str = "guardrail: allow-stub";

/// Escape hatches for the two configuration rules, spelled the same way.
const ALLOW_ENDPOINT: &str = "guardrail: allow-endpoint";
const ALLOW_ID: &str = "guardrail: allow-id";

/// Comments this scan never reads: machine directives and tracked debt.
const DIRECTIVES: &[&str] = &[
    "noqa",
    "type:",
    "pragma",
    "fmt:",
    "ruff:",
    "mypy:",
    "pylint:",
    "nosec",
    "guardrail:",
    "isort:",
    "black:",
    "flake8:",
    "coding:",
    "pyright:",
    "region",
    "endregion",
];
const DEBT_LABELS: &[&str] = &["todo", "fixme", "xxx", "hack"];

/// A comment that narrates the act of writing rather than the code. Measured
/// against the tree: ordinal openers ("first", "then", "next") are not a
/// narration signal here -- they overwhelmingly describe *data* ("First 7
/// positions unchanged"), so only a first-person subject counts.
const PRONOUNS: &[&str] = &["we", "i", "let\'s", "lets"];
const NARRATIVE_PHRASES: &[&str] = &[
    "as you can see",
    "note that we",
    "here we",
    "this is where we",
    "what we do here",
];

/// A comment about the edit rather than about the code.
/// Only verbs that cannot be read as an adjective or as a statement about
/// behaviour. "fixed" is a fixed seed far more often than a repair, "changing"
/// opens a conditional ("Changing the final token may affect ..."), and
/// "keeping"/"leaving" open design notes.
const META_OPENERS: &[&str] = &[
    "added",
    "adding",
    "removed",
    "removing",
    "updated",
    "updating",
    "renamed",
    "refactored",
    "reverted",
    "deleted",
];
const META_PHRASES: &[&str] = &[
    "as requested",
    "per the request",
    "per your request",
    "as you asked",
    "as asked",
    "no change needed",
    "no changes needed",
    "same as above",
    "same as before",
];

/// Dropped before a comment is compared with the code beneath it: they carry no
/// information either way, so leaving them in would hide a pure restatement
/// behind one filler word.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "for", "in", "on", "at", "is", "are", "be", "this",
    "that", "it", "its", "we", "then", "so", "if", "not", "with", "from", "by", "as", "into",
    "out", "all", "any", "each", "here", "no",
];

struct Source<'a> {
    src: &'a str,
    lines: Vec<&'a str>,
}

impl<'a> Source<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            lines: src.lines().collect(),
        }
    }

    fn text(&self, node: Node) -> &'a str {
        &self.src[node.byte_range()]
    }

    fn line_of(&self, node: Node) -> usize {
        node.start_position().row + 1
    }

    /// One-based, because every finding this module emits is one-based.
    fn line(&self, number: usize) -> &'a str {
        if number == 0 {
            return "";
        }
        self.lines.get(number - 1).copied().unwrap_or("")
    }

    fn own_line(&self, node: Node) -> bool {
        let column = node.start_position().column;
        match self.line(self.line_of(node)).get(..column) {
            Some(prefix) => prefix.trim().is_empty(),
            None => false,
        }
    }

    /// The escape hatches sit on the definition line or the line above it, so a
    /// waiver stays next to the thing it waives instead of drifting up the file.
    fn suppressed(&self, line: usize, token: &str) -> bool {
        self.line(line).contains(token) || self.line(line.saturating_sub(1)).contains(token)
    }
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect()
}

/// Split an identifier the way a reader does: `parse_hunks` and `parseHunks`
/// both become {parse, hunks}, so a comment restating either is caught.
fn identifier_words(token: &str, out: &mut BTreeSet<String>) {
    let mut current = String::new();
    let mut previous_lower = false;
    for ch in token.chars() {
        if ch == '_' || !ch.is_alphanumeric() {
            if !current.is_empty() {
                out.insert(std::mem::take(&mut current).to_lowercase());
            }
            previous_lower = false;
            continue;
        }
        if ch.is_uppercase() && previous_lower && !current.is_empty() {
            out.insert(std::mem::take(&mut current).to_lowercase());
        }
        previous_lower = ch.is_lowercase() || ch.is_numeric();
        current.push(ch);
    }
    if !current.is_empty() {
        out.insert(current.to_lowercase());
    }
}

fn code_words(line: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for token in line.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if !token.is_empty() {
            identifier_words(token, &mut out);
        }
    }
    out
}

fn comment_body(text: &str) -> &str {
    text.trim_start_matches('#').trim()
}

/// True for comments this scan declines to judge: shebangs, machine directives,
/// tracked debt and dividers. Reading them would only produce noise, and a
/// noisy check is one people learn to bypass.
fn ignorable(body: &str, raw: &str) -> bool {
    if raw.starts_with("#!") || body.is_empty() {
        return true;
    }
    if !body.chars().any(|c| c.is_alphanumeric()) {
        return true;
    }
    if is_divider(body) || carries_notation(body) || carries_a_date(body) {
        return true;
    }
    let lowered = body.to_lowercase();
    if DIRECTIVES.iter().any(|d| lowered.starts_with(d)) {
        return true;
    }
    let first = words(&lowered).into_iter().next().unwrap_or_default();
    DEBT_LABELS.contains(&first.as_str())
}

/// A section rule like `── extract_block ───────`. The name inside it is a
/// heading, not a sentence, and comparing it with the code below is meaningless.
fn is_divider(body: &str) -> bool {
    let edge = |mut chars: Box<dyn Iterator<Item = char>>| -> bool {
        let first = match chars.next() {
            Some(c) if !c.is_alphanumeric() && !c.is_whitespace() => c,
            _ => return false,
        };
        chars.next() == Some(first)
    };
    edge(Box::new(body.chars())) || edge(Box::new(body.chars().rev()))
}

/// Shapes and maths -- `x: (B, S, D) -> (B, S, 1, D)`, `i in [0, n)`. These are
/// the densest comments in the tree and none of them is prose.
fn carries_notation(body: &str) -> bool {
    body.contains('[')
        || body.contains("==")
        || body.contains("->")
        || body.contains('@')
        || body.contains('%')
        || body.chars().any(|c| {
            "\u{2192}\u{2208}\u{2248}\u{2264}\u{2265}\u{00d7}\u{2211}\u{220f}\u{00b1}\u{2297}"
                .contains(c)
        })
}

/// An ISO date makes a comment a decision record -- "REMOVED 2026-08-02 (user
/// directive): ..." is exactly the archaeology this repo wants kept. Undated
/// edit chatter is what the rule is for.
fn carries_a_date(body: &str) -> bool {
    let bytes = body.as_bytes();
    bytes.windows(10).any(|w| {
        w[..4].iter().all(u8::is_ascii_digit)
            && w[4] == b'-'
            && w[5..7].iter().all(u8::is_ascii_digit)
            && w[7] == b'-'
            && w[8..].iter().all(u8::is_ascii_digit)
    })
}

fn contains_phrase(terms: &[String], phrases: &[&str]) -> bool {
    let joined = format!(" {} ", terms.join(" "));
    phrases.iter().any(|p| joined.contains(&format!(" {p} ")))
}

fn narrates(_body: &str, terms: &[String]) -> bool {
    if contains_phrase(terms, NARRATIVE_PHRASES) {
        return true;
    }
    // A first-person subject up front -- "Now we walk the list", "We will then
    // ..." -- is the session narrating itself. Later in the sentence it is
    // usually ordinary prose ("find where we left off"), so the window is tight.
    terms
        .iter()
        .take(3)
        .any(|word| PRONOUNS.contains(&word.as_str()))
}

fn describes_the_edit(terms: &[String]) -> bool {
    if contains_phrase(terms, META_PHRASES) {
        return true;
    }
    matches!(terms.first().map(String::as_str), Some(f) if META_OPENERS.contains(&f))
}

/// The next line carrying code, skipping blanks and further comments.
fn code_line_after<'a>(source: &Source<'a>, line: usize) -> Option<(usize, &'a str)> {
    for number in (line + 1)..=source.lines.len() {
        let text = source.line(number).trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        return Some((number, source.line(number)));
    }
    None
}

fn scan_comments(source: &Source, comments: &[Node], path: &str, out: &mut Vec<Finding>) {
    let own: Vec<&Node> = comments.iter().filter(|n| source.own_line(**n)).collect();
    let own_lines: BTreeSet<usize> = own.iter().map(|n| source.line_of(**n)).collect();

    for node in comments {
        let raw = source.text(*node);
        let body = comment_body(raw);
        if ignorable(body, raw) {
            continue;
        }
        let terms = words(body);
        let line = source.line_of(*node);

        // A paragraph is an explanation; only its opening line can be narration,
        // and a continuation line restating code is just how prose wraps.
        let continues_a_block = own_lines.contains(&line.wrapping_sub(1));

        if !continues_a_block && narrates(body, &terms) {
            out.push(Finding {
                path: path.to_string(),
                line,
                rule: "comment/effort-narrative",
                message: format!(
                    "comment narrates the work rather than the code: {:?}",
                    truncate(body)
                ),
            });
            continue;
        }
        if !continues_a_block && describes_the_edit(&terms) {
            out.push(Finding {
                path: path.to_string(),
                line,
                rule: "comment/change-meta",
                message: format!(
                    "comment describes the edit, not the code -- git already records it: {:?}",
                    truncate(body)
                ),
            });
            continue;
        }

        // Restatement is only decidable for a lone comment sitting on its own
        // line above one statement. Inside a paragraph the following line is not
        // what the sentence is about.
        if continues_a_block || own_lines.contains(&(line + 1)) || !source.own_line(*node) {
            continue;
        }
        let content: Vec<String> = terms
            .iter()
            .filter(|w| !STOPWORDS.contains(&w.as_str()) && w.len() >= 3)
            .cloned()
            .collect();
        if content.len() < 2 {
            continue;
        }
        let Some((_, code)) = code_line_after(source, line) else {
            continue;
        };
        let vocabulary = code_words(code);
        if content.iter().all(|word| vocabulary.contains(word)) {
            out.push(Finding {
                path: path.to_string(),
                line,
                rule: "comment/trivial-restatement",
                message: format!(
                    "comment repeats the line below it in words: {:?}",
                    truncate(body)
                ),
            });
        }
    }
}

fn truncate(body: &str) -> String {
    if body.chars().count() <= 72 {
        return body.to_string();
    }
    let head: String = body.chars().take(69).collect();
    format!("{head}...")
}

fn named<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn statements<'t>(block: Node<'t>) -> Vec<Node<'t>> {
    named(block)
        .into_iter()
        .filter(|n| n.kind() != "comment")
        .collect()
}

fn decorators<'t>(func: Node<'t>) -> Vec<Node<'t>> {
    let Some(parent) = func.parent() else {
        return Vec::new();
    };
    if parent.kind() != "decorated_definition" {
        return Vec::new();
    }
    named(parent)
        .into_iter()
        .filter(|n| n.kind() == "decorator")
        .collect()
}

/// Decorators that make an empty body the point of the function rather than an
/// omission: the body is supplied elsewhere, or there is deliberately none.
const INTERFACE_DECORATORS: &[&str] = &[
    "abstractmethod",
    "abstractproperty",
    "abc.abstract",
    "overload",
    "typing.overload",
];

fn func_name<'a>(source: &Source<'a>, func: Node) -> &'a str {
    func.child_by_field_name("name")
        .map(|n| source.text(n))
        .unwrap_or("")
}

/// A body that supplies no behaviour: nothing, `pass`, `...`, a docstring, or
/// any mix of those.
fn is_stub(block: Node) -> bool {
    let body = statements(block);
    if body.is_empty() {
        return true;
    }
    body.iter().all(|statement| match statement.kind() {
        "pass_statement" => true,
        "expression_statement" => {
            let inner = named(*statement);
            inner.len() == 1
                && matches!(
                    inner[0].kind(),
                    "ellipsis" | "string" | "concatenated_string" | "none"
                )
        }
        _ => false,
    })
}

/// True when the function's own class declares a structural type, where a body
/// would be the surprise.
fn inside_a_protocol(source: &Source, func: Node) -> bool {
    let mut node = func.parent();
    while let Some(current) = node {
        if current.kind() == "class_definition" {
            return current
                .child_by_field_name("superclasses")
                .map(|bases| source.text(bases).contains("Protocol"))
                .unwrap_or(false);
        }
        node = current.parent();
    }
    false
}

fn scan_function(source: &Source, func: Node, path: &str, out: &mut Vec<Finding>) {
    let Some(block) = func.child_by_field_name("body") else {
        return;
    };
    let line = source.line_of(func);
    let name = func_name(source, func);
    let marks = decorators(func);
    let decorated: String = marks.iter().map(|d| source.text(*d)).collect();

    if is_stub(block) {
        let declared = INTERFACE_DECORATORS.iter().any(|d| decorated.contains(d));
        // A comment inside the body is the author saying why there is nothing to
        // do. That is the documented no-op the rule is trying to get to, not the
        // unfinished stub it is trying to catch.
        let explained = (line + 1..=block.end_position().row + 1)
            .any(|n| source.line(n).trim_start().starts_with('#'));
        if !declared
            && !explained
            && !inside_a_protocol(source, func)
            && !source.suppressed(line, ALLOW_STUB)
            && !path.ends_with(".pyi")
        {
            out.push(Finding {
                path: path.to_string(),
                line,
                rule: "dead/empty-function",
                message: format!(
                    "{name}() has no body: delete it, or raise NotImplementedError so a caller \
                     that reaches it fails loud"
                ),
            });
        }
    }
}

/// Statements that end a block: anything after one in the same block is dead.
const TERMINATORS: &[&str] = &[
    "return_statement",
    "raise_statement",
    "continue_statement",
    "break_statement",
];

fn scan_block(source: &Source, block: Node, path: &str, out: &mut Vec<Finding>) {
    let body = statements(block);
    for (index, statement) in body.iter().enumerate() {
        if !TERMINATORS.contains(&statement.kind()) {
            continue;
        }
        if let Some(next) = body.get(index + 1) {
            out.push(Finding {
                path: path.to_string(),
                line: source.line_of(*next),
                rule: "dead/unreachable-statement",
                message: format!(
                    "unreachable: the {} on line {} always leaves this block",
                    statement.kind().replace('_', " "),
                    source.line_of(*statement)
                ),
            });
        }
        break;
    }
}

/// Literals that decide a branch before the program runs. `while True` is the
/// idiomatic loop and is deliberately absent.
fn constant_condition(kind: &str) -> bool {
    matches!(
        kind,
        "true" | "false" | "integer" | "float" | "string" | "concatenated_string" | "none"
    )
}

fn scan_condition(source: &Source, node: Node, path: &str, out: &mut Vec<Finding>) {
    let keyword = match node.kind() {
        "if_statement" => "if",
        "elif_clause" => "elif",
        "while_statement" => "while",
        _ => return,
    };
    let Some(condition) = node.child_by_field_name("condition") else {
        return;
    };
    if keyword == "while" && condition.kind() != "false" {
        return;
    }
    if !constant_condition(condition.kind()) {
        return;
    }
    out.push(Finding {
        path: path.to_string(),
        line: source.line_of(node),
        rule: "dead/constant-condition",
        message: format!(
            "`{keyword} {}` is decided before the program runs: one branch is dead code",
            truncate(source.text(condition).trim())
        ),
    });
}

/// A path whose contents are test material. A fixture names a host or an id on
/// purpose, and hoisting it to a module constant would say less about the case
/// than the literal sitting in it does.
fn is_test_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.starts_with("test_")
        || name.ends_with("_test.py")
        || name == "conftest.py"
        || path.starts_with("tests/")
        || path.contains("/tests/")
}

/// True when `text` is a URL or a bare IPv4 address, either optionally carrying
/// a port and a path.
///
/// The whole literal has to be the address rather than contain one: a sentence
/// that mentions a URL is documentation, and a literal that *is* one is a name
/// for something outside this repository. Schema and namespace URIs are caught
/// alongside endpoints on purpose -- nothing is connected to, but they are the
/// same kind of external name, and the same one place is where to keep them.
fn is_endpoint(text: &str) -> bool {
    let rest = match text.strip_prefix("https://") {
        Some(rest) => rest,
        None => match text.strip_prefix("http://") {
            Some(rest) => rest,
            None => return is_ipv4_endpoint(text),
        },
    };
    !rest.contains(char::is_whitespace)
        && rest
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
}

/// `127.0.0.1`, `127.0.0.1:7317`, `10.0.0.4/metrics` -- the same address with
/// the scheme left off, which is how a socket call spells it.
fn is_ipv4_endpoint(text: &str) -> bool {
    if text.contains(char::is_whitespace) {
        return false;
    }
    let host = match text.split(['/', ':']).next() {
        Some(host) => host,
        None => return false,
    };
    let mut octets = 0;
    for octet in host.split('.') {
        if !matches!(octet.parse::<u16>(), Ok(value) if value <= 255 && !octet.is_empty()) {
            return false;
        }
        octets += 1;
    }
    octets == 4
}

/// Exactly a canonical UUID: 8-4-4-4-12 hexadecimal digits.
fn is_uuid(text: &str) -> bool {
    const WIDTHS: [usize; 5] = [8, 4, 4, 4, 12];
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == WIDTHS.len()
        && groups.iter().zip(WIDTHS).all(|(group, width)| {
            group.len() == width && group.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// The bytes between the quotes, for a plain literal only.
///
/// A string carrying an interpolation is assembled at run time, and the piece
/// in front of the first `{` is a scheme, not an address.
fn string_content<'a>(source: &Source<'a>, node: Node) -> Option<&'a str> {
    let mut content = None;
    for child in named(node) {
        match child.kind() {
            "interpolation" => return None,
            "string_content" => {
                if content.is_some() {
                    return None;
                }
                content = Some(source.text(child));
            }
            _ => {}
        }
    }
    content
}

/// True when the literal is (part of) the value of a module- or class-level
/// binding, which is exactly where this rule is asking for it to be: named once
/// near the top of the file, where a reader looking for what this module talks
/// to finds it and a deployment can override it in one place.
fn is_named_binding(node: Node) -> bool {
    let mut assigned = false;
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            // A method body is not the top of the file, whatever encloses it.
            "function_definition" | "lambda" => return false,
            "assignment" => assigned = true,
            "module" => return assigned,
            _ => {}
        }
        current = parent;
    }
    false
}

/// A bare string statement: a docstring, or prose left standing. Either way it
/// is being read, not connected to.
fn is_bare_string(node: Node) -> bool {
    node.parent()
        .is_some_and(|parent| parent.kind() == "expression_statement")
}

fn scan_literal(source: &Source, node: Node, path: &str, out: &mut Vec<Finding>) {
    if is_bare_string(node) || is_named_binding(node) {
        return;
    }
    let Some(text) = string_content(source, node) else {
        return;
    };
    let line = source.line_of(node);
    if is_endpoint(text) {
        if source.suppressed(line, ALLOW_ENDPOINT) {
            return;
        }
        out.push(Finding {
            path: path.to_string(),
            line,
            rule: "config/hardcoded-endpoint",
            message: format!(
                "`{}` is a URL written into the call that uses it: bind it to a \
                 module-level constant, so what this module talks to is visible in \
                 one place and can be pointed elsewhere without editing this line \
                 -- or write down why with `# {ALLOW_ENDPOINT}`",
                truncate(text)
            ),
        });
        return;
    }
    if is_uuid(text) && !source.suppressed(line, ALLOW_ID) {
        out.push(Finding {
            path: path.to_string(),
            line,
            rule: "config/hardcoded-id",
            message: format!(
                "`{}` is a fixed identifier with no name on it: bind it to a \
                 module-level constant that says what it identifies -- or write down \
                 why with `# {ALLOW_ID}`",
                truncate(text)
            ),
        });
    }
}

/// Walk the tree once, dispatching each node to the rules that care about it.
pub fn scan_source(path: &str, source_text: &str) -> Result<Vec<Finding>, String> {
    let tree =
        parse(source_text).ok_or_else(|| format!("{path}: could not be parsed as Python"))?;
    // tree-sitter is error-tolerant: it hands back a tree with ERROR nodes
    // rather than refusing. Walking that tree would report no findings for the
    // one file most likely to be broken, which is a pass this check must never
    // give.
    if tree.root_node().has_error() {
        return Err(format!("{path}: is not valid Python"));
    }
    let source = Source::new(source_text);
    let mut findings = Vec::new();
    let mut comments = Vec::new();

    // Test material names hosts and ids on purpose, so the configuration rules
    // stand down there; every other rule in this module applies everywhere.
    let configuration = !is_test_path(path);

    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "comment" => comments.push(node),
            "string" if configuration => scan_literal(&source, node, path, &mut findings),
            "function_definition" => scan_function(&source, node, path, &mut findings),
            "block" => scan_block(&source, node, path, &mut findings),
            "if_statement" | "elif_clause" | "while_statement" => {
                scan_condition(&source, node, path, &mut findings)
            }
            _ => {}
        }
        stack.extend(named(node));
    }

    comments.sort_by_key(|n| n.start_byte());
    scan_comments(&source, &comments, path, &mut findings);
    findings.sort();
    Ok(findings)
}

/// Scan Python files and return one dict per finding.
///
/// `repo`, when given, is stripped from each path so findings are addressed the
/// way the gate and a reviewer both name files.
#[pyfunction]
#[pyo3(signature = (paths, repo=None))]
pub fn style_scan_files(
    py: Python<'_>,
    paths: Vec<String>,
    repo: Option<String>,
) -> PyResult<Py<PyList>> {
    let root = repo.map(PathBuf::from);
    let mut findings = Vec::new();
    for raw in paths {
        let path = PathBuf::from(&raw);
        let relative = match root.as_ref() {
            Some(root) => path
                .strip_prefix(root)
                .map_err(|_| {
                    PyValueError::new_err(format!(
                        "style scan path {} is outside repository {}",
                        path.display(),
                        root.display()
                    ))
                })?
                .to_string_lossy()
                .replace('\\', "/"),
            None => path.to_string_lossy().replace('\\', "/"),
        };
        let bytes = fs::read(&path).map_err(|error| {
            PyValueError::new_err(format!(
                "style scan cannot read {}: {error}",
                path.display()
            ))
        })?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        findings.extend(scan_source(&relative, &text).map_err(PyValueError::new_err)?);
    }
    findings.sort();

    let out = PyList::empty(py);
    for finding in findings {
        let row = PyDict::new(py);
        row.set_item("path", finding.path)?;
        row.set_item("line", finding.line)?;
        row.set_item("rule", finding.rule)?;
        row.set_item("message", finding.message)?;
        out.append(row)?;
    }
    Ok(out.unbind())
}

/// Every rule this module can emit, so the CLI can document itself and a test
/// can prove the list and the implementation have not drifted apart.
#[pyfunction]
pub fn style_scan_rules() -> Vec<&'static str> {
    vec![
        "comment/change-meta",
        "comment/effort-narrative",
        "comment/trivial-restatement",
        "config/hardcoded-endpoint",
        "config/hardcoded-id",
        "dead/constant-condition",
        "dead/empty-function",
        "dead/unreachable-statement",
    ]
}
