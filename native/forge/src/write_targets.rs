//! Native port of `tooling/hooks/agent/bash_write_targets.py`: extract the repo
//! paths a shell command would write.
//!
//! Ported behaviour-identical, including its tokenizer: Python's
//! `shlex.shlex(text, posix=True, punctuation_chars=True)` with
//! `whitespace_split = True`. `posix_shlex` below is a line-for-line
//! transliteration of `shlex.shlex.read_token` (see cpython's `Lib/shlex.py`),
//! not a reimplementation from a spec -- the punctuation-run grouping (`&&`,
//! `>>`, but `2>&1` -> `["2", ">&", "1"]`), the comment character `#`, and the
//! posix backslash-inside-double-quotes rules all come from that state
//! machine. Divergence there would silently desync every write-target and
//! bash-guard verdict from its Python twin.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

/// Sentinel target for a write whose path is not statically knowable.
pub const OPAQUE_WRITE: &str = "<opaque-interpreter-write>";

const SHELL_RUNNERS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh"];

/// Token boundaries between commands (`split_commands`). Shell keywords are
/// included because a loop body (`for f in ...; do cp "$f" "$d"; done`)
/// otherwise runs on into the next command.
const OPERATORS: &[&str] = &[
    ";", "&&", "||", "|", "&", "\n", "do", "done", "then", "fi", "else", "elif", "esac", "{", "}",
];

const WRITE_ALL_OPERANDS: &[&str] = &["rm", "truncate", "touch", "shred", "unlink"];
const WRITE_LAST_OPERAND: &[&str] = &["cp", "mv", "install", "ln", "rsync"];
const INTERPRETERS: &[&str] = &["python", "python3", "perl", "ruby", "node", "php"];
const NULL_SINKS: &[&str] = &["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"];
const WRITE_REDIRECTS: &[&str] = &[">", ">>", "&>", "&>>"];

fn value_flags(exe: &str) -> &'static [&'static str] {
    match exe {
        "truncate" => &["-s", "-r", "--size", "--reference"],
        "shred" => &["-n", "-s", "--iterations", "--size", "--random-source"],
        "install" => &["-m", "-o", "-g", "-t", "--mode", "--owner", "--group"],
        "cp" => &["-t", "-S", "--target-directory", "--suffix"],
        "mv" => &["-t", "-S", "--target-directory", "--suffix"],
        "ln" => &["-t", "-S", "--target-directory", "--suffix"],
        "rsync" => &["-e", "--rsh", "--exclude", "--include", "--files-from"],
        "tee" => &["-p", "--output-error"],
        _ => &[],
    }
}

const WRITE_METHODS: &str = "write_text|write_bytes|unlink|mkdir|rename|touch";

static INLINE_WRITE_IDIOMS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        open\s*\([^)]*['"][wax]b?\+?['"]
        | \.write_text\s*\(
        | \.write_bytes\s*\(
        | \.unlink\s*\(
        | \.mkdir\s*\(
        | \.rename\s*\(
        | \.replace\s*\(\s*[^)]*Path
        | os\.(?:remove|unlink|rename|replace|truncate|makedirs)\s*\(
        | shutil\.(?:copy|copy2|copyfile|copytree|move|rmtree)\s*\(
        | json\.dump\s*\(
        | \bFile::(?:Copy|Path)\b
        "#,
    )
    .unwrap()
});

static PATH_BINDING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(?:\w+\.)?Path\s*\(\s*['"]([^'"]+)['"]"#).unwrap()
});

static WRITTEN_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"([A-Za-z_][A-Za-z0-9_]*)\.(?:{WRITE_METHODS})\s*\("
    ))
    .unwrap()
});

static DIRECT_WRITE_LITERAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?x)
        (?:\w+\.)?Path\s*\(\s*['"]([^'"]+)['"]\s*\)\s*\.(?:{WRITE_METHODS})\s*\(
        | open\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"][wax]
        | os\.(?:remove|unlink|rename|replace|truncate|makedirs)\s*\(\s*['"]([^'"]+)['"]
        | shutil\.(?:copy|copy2|copyfile|copytree|move|rmtree)\s*\([^,)]*,\s*['"]([^'"]+)['"]
        "#
    ))
    .unwrap()
});

/// Matches Python's `_HEREDOC = re.compile(r"<<-?\s*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1")`
/// at the start of `text` (which must already start with `<<`). The closing
/// quote must equal the opening one, a backreference the `regex` crate can't
/// express, so this is a small hand-rolled scanner instead of one `Regex`.
/// Returns `(delimiter, byte length of the whole match)`, or `None`.
fn match_heredoc_intro(text: &str) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut i = 2usize; // skip "<<"
    if bytes.get(i) == Some(&b'-') {
        i += 1;
    }
    while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
        i += 1;
    }
    let quote = match bytes.get(i) {
        Some(b'\'') | Some(b'"') => {
            let q = bytes[i];
            i += 1;
            Some(q)
        }
        _ => None,
    };
    let name_start = i;
    while bytes
        .get(i)
        .is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'_')
    {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name = text[name_start..i].to_string();
    if let Some(q) = quote {
        if bytes.get(i) == Some(&q) {
            i += 1;
        } else {
            return None; // Python's `\1` group is required when a quote opened
        }
    }
    Some((name, i))
}

static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^|[;&|\n]|\s)([A-Za-z_][A-Za-z0-9_]*)=(?:"([^"]*)"|'([^']*)'|([^\s;&|]+))"#)
        .unwrap()
});

static FOR_LOOP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bfor\s+([A-Za-z_][A-Za-z0-9_]*)\s+in\s+([^;\n]+?)\s*(?:;|\n)\s*do\b").unwrap()
});

static VARIABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)").unwrap()
});

static FALLBACK_WRITE_SHAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(^|[^0-9<>&])>{1,2}\s*[^&\s]|(^|\s)(sed\s+-[a-z]*i|tee|dd\s+of=)\b").unwrap()
});

// ---------------------------------------------------------------------------
// posix_shlex: a transliteration of cpython's shlex.shlex(posix=True,
// punctuation_chars=True).read_token with whitespace_split = True.
// ---------------------------------------------------------------------------
pub mod posix_shlex {
    const WHITESPACE: &str = " \t\r\n";
    const PUNCTUATION_CHARS: &str = "();<>|&";
    const QUOTES: &str = "'\"";
    const WORDCHARS_UNICODE: &str =
        "ßàáâãäåæçèéêëìíîïðñòóôõöøùúûüýþÿÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖØÙÚÛÜÝÞ";
    const WORDCHARS_EXTRA: &str = "~-./*?=";

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum St {
        Start,
        Word,
        Punct,
        Quote(char),
        Escape,
    }

    fn is_ws(c: char) -> bool {
        WHITESPACE.contains(c)
    }
    fn is_punct(c: char) -> bool {
        PUNCTUATION_CHARS.contains(c)
    }
    fn is_quote(c: char) -> bool {
        QUOTES.contains(c)
    }
    fn is_wordchar(c: char) -> bool {
        c.is_ascii_alphanumeric()
            || c == '_'
            || WORDCHARS_UNICODE.contains(c)
            || WORDCHARS_EXTRA.contains(c)
    }

    /// `shlex.shlex(command, posix=True, punctuation_chars=True)` with
    /// `whitespace_split = True`; `list(lexer)`. Returns `Err` for the two
    /// `ValueError`s cpython's shlex raises ("No closing quotation" and "No
    /// escaped character") -- callers fall back the same way the Python
    /// callers do.
    pub fn tokenize(command: &str) -> Result<Vec<String>, &'static str> {
        let chars: Vec<char> = command.chars().collect();
        let mut pos = 0usize;
        let mut pushback: Option<char> = None;
        let mut tokens = Vec::new();

        while let Some(tok) = read_token(&chars, &mut pos, &mut pushback)? {
            tokens.push(tok);
        }
        Ok(tokens)
    }

    fn next_char(chars: &[char], pos: &mut usize, pushback: &mut Option<char>) -> Option<char> {
        if let Some(c) = pushback.take() {
            return Some(c);
        }
        if *pos < chars.len() {
            let c = chars[*pos];
            *pos += 1;
            Some(c)
        } else {
            None
        }
    }

    fn consume_comment(chars: &[char], pos: &mut usize) {
        while *pos < chars.len() && chars[*pos] != '\n' {
            *pos += 1;
        }
        if *pos < chars.len() {
            *pos += 1; // consume the newline itself
        }
    }

    // The three `state = St::Start` assignments immediately before a `break` in
    // the `Punct` arm mirror cpython's `self.state = ' '` in the same spots
    // (`Lib/shlex.py`'s `read_token`, state `'c'`) -- kept for fidelity to the
    // reference even though this function's `state` is local and never read
    // again after `break`.
    #[allow(unused_assignments)]
    fn read_token(
        chars: &[char],
        pos: &mut usize,
        pushback: &mut Option<char>,
    ) -> Result<Option<String>, &'static str> {
        let mut token = String::new();
        let mut quoted = false;
        let mut state = St::Start;
        // Only ever Word or Quote(_): where an escape returns to.
        let mut escapedstate = St::Word;

        loop {
            let nextchar = next_char(chars, pos, pushback);
            match state {
                St::Start => match nextchar {
                    None => break,
                    Some(c) if is_ws(c) => {
                        if !token.is_empty() || quoted {
                            break;
                        }
                        continue;
                    }
                    Some('#') => {
                        consume_comment(chars, pos);
                        continue;
                    }
                    Some('\\') => {
                        escapedstate = St::Word;
                        state = St::Escape;
                    }
                    Some(c) if is_wordchar(c) => {
                        token.push(c);
                        state = St::Word;
                    }
                    Some(c) if is_punct(c) => {
                        token.push(c);
                        state = St::Punct;
                    }
                    Some(c) if is_quote(c) => {
                        state = St::Quote(c);
                    }
                    Some(c) => {
                        // whitespace_split catches everything else.
                        token.push(c);
                        state = St::Word;
                    }
                },
                St::Quote(q) => {
                    quoted = true;
                    match nextchar {
                        None => return Err("No closing quotation"),
                        Some(c) if c == q => {
                            state = St::Word;
                        }
                        Some('\\') if q == '"' => {
                            escapedstate = St::Quote(q);
                            state = St::Escape;
                        }
                        Some(c) => token.push(c),
                    }
                }
                St::Escape => match nextchar {
                    None => return Err("No escaped character"),
                    Some(c) => {
                        if let St::Quote(q) = escapedstate {
                            if c != '\\' && c != q {
                                token.push('\\');
                            }
                            token.push(c);
                        } else {
                            token.push(c);
                        }
                        state = escapedstate;
                    }
                },
                St::Word => match nextchar {
                    None => break,
                    Some(c) if is_ws(c) => {
                        state = St::Start;
                        if !token.is_empty() || quoted {
                            break;
                        }
                        continue;
                    }
                    Some('#') => {
                        consume_comment(chars, pos);
                        state = St::Start;
                        if !token.is_empty() || quoted {
                            break;
                        }
                        continue;
                    }
                    Some(c) if is_quote(c) => {
                        state = St::Quote(c);
                    }
                    Some('\\') => {
                        escapedstate = St::Word;
                        state = St::Escape;
                    }
                    Some(c) if !is_punct(c) => {
                        // wordchars, quotes (handled above) or
                        // (whitespace_split && not punctuation_chars).
                        token.push(c);
                    }
                    Some(c) => {
                        *pushback = Some(c);
                        state = St::Start;
                        if !token.is_empty() || quoted {
                            break;
                        }
                        continue;
                    }
                },
                St::Punct => match nextchar {
                    None => break,
                    Some(c) if is_ws(c) => {
                        state = St::Start;
                        break;
                    }
                    Some('#') => {
                        consume_comment(chars, pos);
                        state = St::Start;
                        break;
                    }
                    Some(c) if is_punct(c) => token.push(c),
                    Some(c) => {
                        if !is_ws(c) {
                            *pushback = Some(c);
                        }
                        state = St::Start;
                        break;
                    }
                },
            }
        }

        if token.is_empty() && !quoted {
            Ok(None)
        } else {
            Ok(Some(token))
        }
    }
}

/// Split a flat token stream into individual commands at shell operators.
pub fn split_commands(tokens: &[String]) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for token in tokens {
        if OPERATORS.contains(&token.as_str()) {
            if !current.is_empty() {
                commands.push(std::mem::take(&mut current));
            }
        } else {
            current.push(token.clone());
        }
    }
    if !current.is_empty() {
        commands.push(current);
    }
    commands
}

fn basename(exe: &str) -> &str {
    exe.rsplit('/').next().unwrap_or(exe)
}

/// Replace unquoted newlines with `;` so each line stays its own command.
fn separate_lines(command: &str) -> String {
    let chars: Vec<char> = command.chars().collect();
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut index = 0usize;
    while index < chars.len() {
        let c = chars[index];
        if let Some(q) = quote {
            out.push(c);
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' && index + 1 < chars.len() {
                index += 1;
                out.push(chars[index]);
            }
        } else if c == '\\' && index + 1 < chars.len() {
            out.push(c);
            index += 1;
            out.push(chars[index]);
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            out.push(c);
        } else if c == '\n' {
            out.push(';');
        } else {
            out.push(c);
        }
        index += 1;
    }
    out
}

/// Heredoc delimiters introduced OUTSIDE a quoted span on one line, plus the
/// exit quote state.
fn heredoc_delimiters(line: &str, mut quote: Option<char>) -> (Vec<String>, Option<char>) {
    let chars: Vec<char> = line.chars().collect();
    let mut delimiters = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        let c = chars[index];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' {
                index += 1;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == '\\' {
            index += 1;
        } else if c == '<' && chars.get(index + 1) == Some(&'<') {
            let rest: String = chars[index..].iter().collect();
            if let Some((name, byte_len)) = match_heredoc_intro(&rest) {
                let char_len = rest[..byte_len].chars().count();
                delimiters.push(name);
                index += char_len - 1;
            }
        }
        index += 1;
    }
    (delimiters, quote)
}

/// Split `command` into (command without heredoc bodies, [(intro, body)]).
fn split_heredocs(command: &str) -> (String, Vec<(String, String)>) {
    let lines: Vec<&str> = command.split('\n').collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut bodies: Vec<(String, String)> = Vec::new();
    let mut quote: Option<char> = None;
    let mut index = 0usize;
    while index < lines.len() {
        let intro = lines[index];
        kept.push(intro);
        let (delimiters, new_quote) = heredoc_delimiters(intro, quote);
        quote = new_quote;
        index += 1;
        for delimiter in delimiters {
            let mut body: Vec<&str> = Vec::new();
            while index < lines.len() && lines[index].trim() != delimiter {
                body.push(lines[index]);
                index += 1;
            }
            index += 1; // consume the terminator itself
            bodies.push((intro.to_string(), body.join("\n")));
        }
    }
    (kept.join("\n"), bodies)
}

fn interpreter_basename_matches(exe: &str) -> bool {
    let trimmed = exe.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    INTERPRETERS
        .iter()
        .any(|name| name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.') == trimmed)
}

/// Write targets hidden in a heredoc that is piped into an interpreter.
fn heredoc_write_targets(bodies: &[(String, String)]) -> Vec<String> {
    let mut targets = Vec::new();
    for (intro, body) in bodies {
        let first: Vec<&str> = intro.split_whitespace().collect();
        let Some(exe_raw) = first.first() else {
            continue;
        };
        let exe = basename(exe_raw);
        if SHELL_RUNNERS.contains(&exe) {
            targets.extend(write_targets_inner(body, false));
            continue;
        }
        if !interpreter_basename_matches(exe) {
            continue;
        }
        if !INLINE_WRITE_IDIOMS.is_match(body) {
            continue;
        }
        let literals = path_literals(body);
        if literals.is_empty() {
            targets.push(OPAQUE_WRITE.to_string());
        } else {
            targets.extend(literals);
        }
    }
    targets
}

/// `NAME=value` bindings made by the command itself.
fn assignments(command: &str) -> HashMap<String, String> {
    let mut bindings = HashMap::new();
    for caps in ASSIGNMENT.captures_iter(command) {
        let name = caps.get(1).unwrap().as_str().to_string();
        let value = caps
            .get(2)
            .or_else(|| caps.get(3))
            .or_else(|| caps.get(4))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        bindings.insert(name, value);
    }
    bindings
}

fn substitute_variable(text: &str, bindings: &HashMap<String, String>) -> String {
    VARIABLE
        .replace_all(text, |caps: &regex::Captures| {
            let name = caps
                .get(1)
                .or_else(|| caps.get(2))
                .map(|m| m.as_str())
                .unwrap_or("");
            bindings
                .get(name)
                .cloned()
                .unwrap_or_else(|| "\0".to_string())
        })
        .into_owned()
}

/// `for NAME in a b c` bindings, when every element resolves to a literal.
fn loop_bindings(
    command: &str,
    bindings: &HashMap<String, String>,
) -> HashMap<String, Vec<String>> {
    let mut loops = HashMap::new();
    for caps in FOR_LOOP.captures_iter(command) {
        let name = caps.get(1).unwrap().as_str().to_string();
        let listing_raw = caps.get(2).unwrap().as_str();
        let listing = substitute_variable(listing_raw, bindings);
        let values: Vec<&str> = listing.split_whitespace().collect();
        if values
            .iter()
            .any(|v| v.contains('\0') || v.contains('$') || v.contains('*') || v.contains('`'))
        {
            continue;
        }
        let cleaned: Vec<String> = values
            .iter()
            .map(|v| v.trim_matches(|c| c == '"' || c == '\'').to_string())
            .collect();
        loops.insert(name, cleaned);
    }
    loops
}

/// Substitute `$VAR` in a redirect target, or return `OPAQUE_WRITE`.
fn expand(target: &str, bindings: &HashMap<String, String>) -> String {
    if !target.contains('$') {
        return target.to_string();
    }
    let resolved = substitute_variable(target, bindings);
    if resolved.contains('\0') {
        OPAQUE_WRITE.to_string()
    } else {
        resolved
    }
}

/// Every path `target` can denote, expanding loop variables to their values.
fn expand_all(
    target: &str,
    bindings: &HashMap<String, String>,
    loops: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut pending = vec![target.to_string()];
    for (name, values) in loops {
        let dollar = format!("${name}");
        let braced = format!("${{{name}}}");
        if !pending
            .iter()
            .any(|candidate| candidate.contains(&dollar) || candidate.contains(&braced))
        {
            continue;
        }
        let mut next = Vec::new();
        for candidate in &pending {
            for value in values {
                next.push(candidate.replace(&braced, value).replace(&dollar, value));
            }
        }
        pending = next;
    }
    pending.iter().map(|c| expand(c, bindings)).collect()
}

/// Literal paths this source actually WRITES, in first-seen order.
fn path_literals(source: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for caps in DIRECT_WRITE_LITERAL.captures_iter(source) {
        for i in 1..caps.len() {
            if let Some(m) = caps.get(i) {
                let value = m.as_str().to_string();
                if seen.insert(value.clone()) {
                    found.push(value);
                }
            }
        }
    }
    let mut bindings: HashMap<String, String> = HashMap::new();
    for caps in PATH_BINDING.captures_iter(source) {
        bindings.insert(
            caps.get(1).unwrap().as_str().to_string(),
            caps.get(2).unwrap().as_str().to_string(),
        );
    }
    for caps in WRITTEN_NAME.captures_iter(source) {
        let name = caps.get(1).unwrap().as_str();
        if let Some(bound) = bindings.get(name) {
            if seen.insert(bound.clone()) {
                found.push(bound.clone());
            }
        }
    }
    found
}

/// Paths written by a file-opening redirect inside one command's argv.
fn redirect_targets(argv: &[String]) -> Vec<String> {
    let mut targets = Vec::new();
    for (position, token) in argv.iter().enumerate() {
        if !WRITE_REDIRECTS.contains(&token.as_str()) {
            continue;
        }
        if let Some(next) = argv.get(position + 1) {
            targets.push(next.clone());
        }
    }
    targets
}

/// Drop redirect operators and their operands from an argv.
fn strip_redirects(argv: &[String]) -> Vec<String> {
    let mut cleaned = Vec::new();
    let mut skip = false;
    for token in argv {
        if skip {
            skip = false;
            continue;
        }
        if WRITE_REDIRECTS.contains(&token.as_str())
            || matches!(token.as_str(), "<" | ">&" | "&>" | "<<" | "<<-")
        {
            skip = true;
            continue;
        }
        cleaned.push(token.clone());
    }
    cleaned
}

/// Non-flag arguments, stopping flag parsing at `--`.
fn operands(args: &[String], exe: &str) -> Vec<String> {
    let takes_value = value_flags(exe);
    let mut out = Vec::new();
    let mut flags_done = false;
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if !flags_done && arg == "--" {
            flags_done = true;
            continue;
        }
        if !flags_done && arg.starts_with('-') && arg != "-" {
            skip = takes_value.contains(&arg.as_str());
            continue;
        }
        out.push(arg.clone());
    }
    out
}

/// The inline program text of an interpreter invocation, if any.
fn inline_source(exe: &str, args: &[String]) -> Option<String> {
    if !interpreter_basename_matches(basename(exe)) {
        return None;
    }
    for flag in ["-c", "-e"] {
        if let Some(index) = args.iter().position(|a| a == flag) {
            if let Some(value) = args.get(index + 1) {
                return Some(value.clone());
            }
        }
    }
    None
}

/// Write targets for a single command's argv.
fn command_targets(argv: &[String]) -> Vec<String> {
    if argv.is_empty() {
        return Vec::new();
    }
    let mut targets = redirect_targets(argv);
    let argv = strip_redirects(argv);
    if argv.is_empty() {
        return targets;
    }

    let exe = basename(&argv[0]).to_string();
    let args = &argv[1..];

    if SHELL_RUNNERS.contains(&exe.as_str()) {
        if let Some(index) = args.iter().position(|a| a == "-c") {
            if let Some(value) = args.get(index + 1) {
                targets.extend(write_targets_inner(value, false));
            }
            return targets;
        }
    }

    if let Some(inline) = inline_source(&exe, args) {
        if INLINE_WRITE_IDIOMS.is_match(&inline) {
            let literals = path_literals(&inline);
            if literals.is_empty() {
                targets.push(OPAQUE_WRITE.to_string());
            } else {
                targets.extend(literals);
            }
            return targets;
        }
    }

    if WRITE_ALL_OPERANDS.contains(&exe.as_str()) {
        targets.extend(operands(args, &exe));
    } else if WRITE_LAST_OPERAND.contains(&exe.as_str()) {
        let ops = operands(args, &exe);
        if ops.len() >= 2 {
            targets.push(ops[ops.len() - 1].clone());
        }
    } else if exe == "sed"
        && (args
            .iter()
            .any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains('i'))
            || args.iter().any(|a| a.starts_with("--in-place")))
    {
        let ops = operands(args, "");
        if args.iter().any(|a| a == "-e" || a == "-f") {
            targets.extend(ops);
        } else if !ops.is_empty() {
            targets.extend(ops[1..].to_vec());
        }
    } else if exe == "tee" {
        targets.extend(operands(args, &exe));
    } else if exe == "dd" {
        targets.extend(
            args.iter()
                .filter(|a| a.starts_with("of="))
                .map(|a| a[3..].to_string()),
        );
    } else if exe == "git" && !args.is_empty() {
        let sub = args[0].as_str();
        if matches!(sub, "apply" | "checkout" | "restore" | "stash") {
            targets.extend(operands(&args[1..], ""));
        }
    } else if exe == "patch" {
        targets.extend(operands(args, ""));
    }

    targets
}

fn tokenize_or_fallback(stripped: &str) -> (Vec<Vec<String>>, bool) {
    match posix_shlex::tokenize(stripped) {
        Ok(tokens) => (split_commands(&tokens), false),
        Err(_) => (Vec::new(), true),
    }
}

/// Return the paths `command` would write, in first-seen order. Returns
/// `[OPAQUE_WRITE]` for a command that is write-shaped but whose target
/// cannot be resolved statically.
pub fn write_targets(command: &str) -> Vec<String> {
    write_targets_inner(command, true)
}

fn write_targets_inner(command: &str, relative: bool) -> Vec<String> {
    let _ = relative; // mirrors the Python signature; both callers behave the same here
    let (stripped_raw, bodies) = split_heredocs(command);
    let mut found = heredoc_write_targets(&bodies);
    let stripped = separate_lines(&stripped_raw);

    let (commands, unparseable) = tokenize_or_fallback(&stripped);
    if unparseable {
        if FALLBACK_WRITE_SHAPE.is_match(&stripped) {
            found.push(OPAQUE_WRITE.to_string());
        }
    } else {
        for argv in &commands {
            found.extend(command_targets(argv));
        }
    }

    let bindings = assignments(&stripped);
    let loops = loop_bindings(&stripped, &bindings);
    let mut seen = Vec::new();
    let mut seen_set = std::collections::HashSet::new();
    for target in &found {
        for expanded in expand_all(target, &bindings, &loops) {
            if !NULL_SINKS.contains(&expanded.as_str()) && seen_set.insert(expanded.clone()) {
                seen.push(expanded);
            }
        }
    }
    seen
}

/// The directory a relative write target resolves against. Returns `None`
/// when a `cd` target cannot be resolved, so the caller can treat relative
/// targets as opaque instead of resolving them wrongly.
pub fn working_directory(command: &str, repo_root: &Path) -> Option<PathBuf> {
    let (stripped_raw, _) = split_heredocs(command);
    let bindings = assignments(&stripped_raw);
    let stripped = separate_lines(&stripped_raw);
    let commands = match posix_shlex::tokenize(&stripped) {
        Ok(tokens) => split_commands(&tokens),
        Err(_) => return Some(repo_root.to_path_buf()),
    };
    let mut current = repo_root.to_path_buf();
    for argv in &commands {
        if argv.is_empty() || basename(&argv[0]) != "cd" {
            continue;
        }
        let ops = operands(&argv[1..], "");
        let Some(first) = ops.first() else {
            return None; // `cd` with no argument goes home; not resolvable here
        };
        let expanded = expand(first, &bindings);
        if expanded == OPAQUE_WRITE || expanded == "-" {
            return None;
        }
        let candidate = PathBuf::from(&expanded);
        current = if candidate.is_absolute() {
            candidate
        } else {
            current.join(candidate)
        };
    }
    Some(current)
}

/// Write targets that land inside `repo_root`, as repo-relative paths. Paths
/// outside the repo (the session scratchpad, `/tmp`) have nothing to claim
/// and are dropped. `OPAQUE_WRITE` is preserved verbatim.
pub fn repo_write_targets(command: &str, repo_root: &Path) -> Vec<String> {
    let base = working_directory(command, repo_root);
    let mut resolved = Vec::new();
    for target in write_targets(command) {
        if target == OPAQUE_WRITE {
            resolved.push(target);
            continue;
        }
        let path = PathBuf::from(&target);
        let absolute = if path.is_absolute() {
            path
        } else {
            match &base {
                Some(base) => base.join(&path),
                None => {
                    resolved.push(OPAQUE_WRITE.to_string());
                    continue;
                }
            }
        };
        let normalized = normalize_lexically(&absolute);
        if let Ok(rel) = normalized.strip_prefix(repo_root) {
            resolved.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    resolved
}

/// Lexical `..`/`.` collapse (no filesystem access), mirroring `Path.resolve()`
/// closely enough for repo-relative targets that need not exist on disk yet.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rm_and_cp_write_all_or_last_operand() {
        assert_eq!(write_targets("rm a.txt b.txt"), vec!["a.txt", "b.txt"]);
        assert_eq!(write_targets("cp a.txt b.txt"), vec!["b.txt"]);
        assert_eq!(write_targets("cp -r src/ dest/"), vec!["dest/"]);
    }

    #[test]
    fn redirects_and_appends_are_write_targets() {
        assert_eq!(write_targets("echo hi > out.txt"), vec!["out.txt"]);
        assert_eq!(
            write_targets("cat a.txt >> combined.log"),
            vec!["combined.log"]
        );
        assert_eq!(write_targets("echo hi > /dev/null"), Vec::<String>::new());
    }

    #[test]
    fn heredoc_redirected_to_a_file_is_a_write_target() {
        assert_eq!(
            write_targets("cat <<EOF > notes.txt\nhello\nEOF"),
            vec!["notes.txt"]
        );
    }

    #[test]
    fn heredoc_piped_to_an_interpreter_with_an_inline_write_is_detected() {
        let command = "python3 <<'EOF'\nopen('x.txt', 'w').write('hi')\nEOF";
        assert_eq!(write_targets(command), vec!["x.txt"]);
    }

    #[test]
    fn sed_in_place_and_tee_and_dd_are_write_targets() {
        assert_eq!(
            write_targets("sed -i 's/a/b/' config.toml"),
            vec!["config.toml"]
        );
        assert_eq!(write_targets("echo hi | tee -a log.txt"), vec!["log.txt"]);
        assert_eq!(
            write_targets("dd if=/dev/zero of=disk.img bs=1M"),
            vec!["disk.img"]
        );
    }

    #[test]
    fn git_write_forms_are_targets_but_read_forms_are_not() {
        assert_eq!(write_targets("git apply patch.diff"), vec!["patch.diff"]);
        assert_eq!(write_targets("git checkout -- file.txt"), vec!["file.txt"]);
        assert_eq!(write_targets("git status"), Vec::<String>::new());
        assert_eq!(write_targets("git log --oneline"), Vec::<String>::new());
    }

    #[test]
    fn python_c_inline_write_is_detected_with_a_literal_path() {
        assert_eq!(
            write_targets("python -c \"open('out.txt','w').write('x')\""),
            vec!["out.txt"]
        );
    }

    #[test]
    fn python_c_inline_write_without_a_literal_path_is_opaque() {
        assert_eq!(
            write_targets("python -c \"open(sys.argv[1],'w').write('x')\""),
            vec![OPAQUE_WRITE.to_string()]
        );
    }

    #[test]
    fn for_loop_expands_literal_targets() {
        assert_eq!(
            write_targets("for f in a.txt b.txt c.txt; do rm \"$f\"; done"),
            vec!["a.txt", "b.txt", "c.txt"]
        );
    }

    #[test]
    fn unparseable_but_write_shaped_falls_back_to_opaque() {
        assert_eq!(
            write_targets("echo hi > \"unterminated"),
            vec![OPAQUE_WRITE.to_string()]
        );
        // Unparseable and not write-shaped: nothing.
        assert_eq!(write_targets("echo \"unterminated"), Vec::<String>::new());
    }

    #[test]
    fn working_directory_tracks_cd_and_flags_unresolvable_targets() {
        let repo = Path::new("/repo");
        assert_eq!(
            working_directory("cd src && echo hi", repo),
            Some(PathBuf::from("/repo/src"))
        );
        assert_eq!(
            working_directory("echo hi", repo),
            Some(PathBuf::from("/repo"))
        );
        assert_eq!(working_directory("cd", repo), None);
        assert_eq!(working_directory("cd $TARGET_DIR", repo), None);
    }

    #[test]
    fn repo_write_targets_resolves_relative_paths_and_drops_outside_repo() {
        let repo = Path::new("/repo");
        assert_eq!(
            repo_write_targets("cd src && cat file.txt > out.txt", repo),
            vec!["src/out.txt"]
        );
        assert_eq!(
            repo_write_targets("cd /tmp && echo done > note.txt", repo),
            Vec::<String>::new()
        );
        assert_eq!(
            repo_write_targets("rm ../outside.txt", repo),
            Vec::<String>::new()
        );
    }

    #[test]
    fn repo_write_targets_preserves_the_opaque_sentinel() {
        let repo = Path::new("/repo");
        assert_eq!(
            repo_write_targets("python -c \"open(sys.argv[1],'w').write('x')\"", repo),
            vec![OPAQUE_WRITE.to_string()]
        );
    }
}
