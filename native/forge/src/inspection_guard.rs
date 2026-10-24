//! Shared, model-independent bounds for shell-based code exploration.
//!
//! This is a conservative command recognizer, not a shell interpreter. Only
//! actual rg/grep commands with understood arguments are classified. Quoted
//! examples, comments and heredoc data are never interpreted as commands.
use crate::write_targets::posix_shlex;

const MAX_LINES: usize = 80;
const OPTIONAL_STATUS: &str = "For an optional search, preserve failures: rg ... || { rg_status=$?; [ \"$rg_status\" -eq 1 ] || exit \"$rg_status\"; }. Do not use || true or suppress stderr.";

#[derive(Debug)]
struct Segment<'a> {
    text: &'a str,
    after: &'static str,
}

fn delimiter_end(text: &str) -> usize {
    let mut quote = None;
    let mut escaped = false;
    for (index, byte) in text.bytes().enumerate() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' && quote != Some(b'\'') {
            escaped = true;
        } else if quote == Some(byte) {
            quote = None;
        } else if quote.is_none() && matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else if quote.is_none()
            && matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b';' | b'|' | b'&')
        {
            return index;
        }
    }
    text.len()
}

fn heredoc_at(text: &str) -> Option<(String, bool)> {
    let tail = text.strip_prefix("<<")?;
    if tail.starts_with('<') {
        return None;
    }
    let strip_tabs = tail.starts_with('-');
    let tail = tail.trim_start_matches('-').trim_start_matches([' ', '\t']);
    let end = delimiter_end(tail);
    let words = posix_shlex::tokenize(&tail[..end], "").ok()?;
    let delimiter = words.first()?;
    if words.len() != 1 || delimiter.is_empty() || delimiter.contains(['$', '`']) {
        return None;
    }
    Some((delimiter.clone(), strip_tabs))
}

fn skip_heredocs(text: &str, mut index: usize, pending: &mut Vec<(String, bool)>) -> usize {
    for (delimiter, strip_tabs) in pending.drain(..) {
        while index < text.len() {
            let end = text[index..].find('\n').map_or(text.len(), |n| index + n);
            let line = &text[index..end];
            index = (end + 1).min(text.len());
            let candidate = if strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line
            };
            if candidate.trim_end_matches('\r') == delimiter {
                break;
            }
        }
    }
    index
}

fn operator(bytes: &[u8], index: usize) -> Option<(&'static str, usize)> {
    match bytes[index] {
        b'\n' => Some((";", 1)),
        b';' => Some((";", 1)),
        b'(' | b')' => Some((";", 1)),
        b'|' if bytes.get(index + 1) == Some(&b'|') => Some(("||", 2)),
        b'|' => Some(("|", 1)),
        b'&' if bytes.get(index + 1) == Some(&b'&') => Some(("&&", 2)),
        // >& and &> are redirections, not command separators.
        b'&' if index > 0 && matches!(bytes[index - 1], b'>' | b'<') => None,
        b'&' if bytes.get(index + 1) == Some(&b'>') => None,
        b'&' => Some(("&", 1)),
        _ => None,
    }
}

fn shell_segments(command: &str) -> Vec<Segment<'_>> {
    let bytes = command.as_bytes();
    let (mut index, mut start, mut quote) = (0, 0, None);
    let mut segments = Vec::new();
    let mut pending = Vec::new();
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(q) = quote {
            if byte == q {
                quote = None;
            } else if byte == b'\\' && q == b'"' {
                index += 1;
            }
        } else if byte == b'\\' {
            index += 1;
        } else if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
        } else if byte == b'#' && (index == 0 || bytes[index - 1].is_ascii_whitespace()) {
            segments.push(Segment {
                text: &command[start..index],
                after: ";",
            });
            index = command[index..]
                .find('\n')
                .map_or(bytes.len(), |n| index + n);
            start = index;
            continue;
        } else if bytes.get(index..index + 2) == Some(b"<<") {
            if let Some(delimiter) = heredoc_at(&command[index..]) {
                pending.push(delimiter);
            }
            index += if bytes.get(index + 2) == Some(&b'<') {
                2
            } else {
                1
            };
        } else if let Some((after, length)) = operator(bytes, index) {
            if byte == b'\n'
                && command[start..index].trim().is_empty()
                && segments
                    .last()
                    .is_some_and(|last| matches!(last.after, "|" | "&&" | "||"))
            {
                index += length;
                start = index;
                continue;
            }
            segments.push(Segment {
                text: &command[start..index],
                after,
            });
            let newline = byte == b'\n';
            index += length;
            if newline {
                index = skip_heredocs(command, index, &mut pending);
            }
            start = index;
            continue;
        }
        index += 1;
    }
    if start < bytes.len() {
        segments.push(Segment {
            text: &command[start..],
            after: "",
        });
    }
    segments
}

fn argv(text: &str) -> Option<Vec<String>> {
    let mut words = posix_shlex::tokenize(text, posix_shlex::PUNCTUATION_CHARS_FULL).ok()?;
    let prefix = words
        .iter()
        .take_while(|word| {
            matches!(
                word.as_str(),
                "if" | "then" | "elif" | "do" | "!" | "command" | "{"
            ) || word.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            })
        })
        .count();
    words.drain(..prefix);
    (!words.is_empty()).then_some(words)
}

struct Search<'a> {
    patterns: Vec<&'a str>,
    paths: Vec<&'a str>,
    quiet: bool,
}

fn search_args(words: &[String]) -> Option<Search<'_>> {
    let exe = words.first()?.rsplit('/').next()?;
    if !matches!(exe, "rg" | "grep") {
        return None;
    }
    let mut search = Search {
        patterns: Vec::new(),
        paths: Vec::new(),
        quiet: false,
    };
    let (mut index, mut options) = (1, true);
    let files_only = words.iter().any(|w| w == "--files");
    while let Some(word) = words.get(index) {
        let word = word.as_str();
        if matches!(word, "2" | "1") && words.get(index + 1).is_some_and(|w| w.contains('>')) {
            break;
        }
        if matches!(word, ">" | ">>" | "&>" | "&>>") {
            break;
        }
        if options && word == "--" {
            options = false;
        } else if options && matches!(word, "-e" | "--regexp") {
            index += 1;
            search.patterns.push(words.get(index)?.as_str());
        } else if options && matches!(word, "-f" | "--file") {
            // A pattern file's structure is unknown; don't infer its intent.
            return None;
        } else if options && matches!(word, "--files" | "--files-with-matches" | "-l") {
        } else if options && matches!(word, "--quiet" | "--silent" | "-q") {
            search.quiet = true;
        } else if options && takes_value(word) {
            index += 1;
            words.get(index)?;
        } else if options && word.starts_with('-') {
            if let Some(pattern) = word.strip_prefix("--regexp=") {
                search.patterns.push(pattern);
            } else if word.starts_with("--") {
                let known_value = word
                    .split_once('=')
                    .is_some_and(|(flag, _)| takes_value(flag));
                if !known_value && !known_flag(word) {
                    return None;
                }
            } else if word
                .chars()
                .skip(1)
                .all(|c| "niHhSswvFxlorRqaUcI".contains(c))
            {
                search.quiet |= word.contains('q');
            } else if !(word.starts_with("-g") || word.starts_with("-t") || word.starts_with("-m"))
            {
                return None;
            }
        } else if search.patterns.is_empty() && !files_only {
            search.patterns.push(word);
        } else {
            search.paths.push(word);
        }
        index += 1;
    }
    Some(search)
}

fn takes_value(word: &str) -> bool {
    matches!(
        word,
        "-g" | "--glob"
            | "--iglob"
            | "-t"
            | "--type"
            | "-T"
            | "--type-not"
            | "-m"
            | "--max-count"
            | "--max-depth"
            | "--max-columns"
            | "--max-filesize"
            | "--include"
            | "--exclude"
            | "--exclude-dir"
            | "--color"
            | "--encoding"
            | "-A"
            | "-B"
            | "-C"
            | "--after-context"
            | "--before-context"
            | "--context"
    )
}

fn known_flag(word: &str) -> bool {
    matches!(
        word,
        "--line-number"
            | "--no-line-number"
            | "--hidden"
            | "--no-ignore"
            | "--ignore-case"
            | "--case-sensitive"
            | "--smart-case"
            | "--fixed-strings"
            | "--word-regexp"
            | "--line-regexp"
            | "--recursive"
            | "--no-heading"
            | "--heading"
            | "--with-filename"
            | "--no-filename"
            | "--invert-match"
            | "--count"
            | "--count-matches"
            | "--null"
            | "--text"
            | "--multiline"
            | "--pcre2"
            | "--no-messages"
            | "--stats"
    )
}

fn explicit_file(path: &str) -> bool {
    if matches!(
        path.rsplit('/').next(),
        Some("Makefile" | "Dockerfile" | "LICENSE")
    ) {
        return true;
    }
    !path.contains(['*', '?', '$', '`', '['])
        && !path.ends_with('/')
        && path.rsplit('/').next().is_some_and(|name| {
            name.rsplit_once('.').is_some_and(|(stem, extension)| {
                !stem.is_empty() && extension.bytes().any(|c| c.is_ascii_alphabetic())
            })
        })
}

fn graph_inventory(patterns: &[&str]) -> bool {
    patterns.iter().any(|p| {
        [
            "ast_context_tool",
            "query_graph_tool",
            "get_minimal_context_tool",
            "semantic_search_nodes_tool",
            "^(from|import)",
            "^import",
            "^from",
            "^def",
            "def (",
            "def [",
            "@triton",
            "^class",
            "^fn ",
        ]
        .iter()
        .any(|marker| p.contains(marker))
    })
}

fn line_limit(words: &[String]) -> Option<usize> {
    let exe = words.first()?.rsplit('/').next()?;
    if exe == "head" {
        return match words.get(1)?.as_str() {
            "-n" | "--lines" => words.get(2)?.parse().ok(),
            value if value.starts_with("--lines=") => value[8..].parse().ok(),
            value if value.starts_with('-') => value[1..].parse().ok(),
            _ => None,
        };
    }
    if exe == "sed" && words.get(1)?.as_str() == "-n" && words.len() == 3 {
        let range = words[2].strip_prefix("1,")?.strip_suffix('p')?;
        return range.parse().ok();
    }
    None
}

fn redirected(words: &[String]) -> bool {
    words.windows(2).enumerate().any(|(index, w)| {
        matches!(w[0].as_str(), ">" | ">>" | "&>" | "&>>")
            && !(index > 0 && words[index - 1] == "2")
            && !matches!(w[1].as_str(), "/dev/stdout" | "/dev/stderr" | "/dev/tty")
    })
}

fn output_budget(segments: &[Segment<'_>], index: usize, words: &[String]) -> Option<usize> {
    if redirected(words) {
        return Some(0);
    }
    if segments[index].after != "|" {
        return None;
    }
    let mut budget = None;
    for next in &segments[index + 1..] {
        let next_words = argv(next.text)?;
        let summary = next_words.first().is_some_and(|word| word == "wc")
            && next_words
                .iter()
                .skip(1)
                .all(|word| matches!(word.as_str(), "-l" | "-w" | "-c" | "-m"));
        if summary || redirected(&next_words) {
            return Some(0);
        }
        if let Some(limit) = line_limit(&next_words) {
            budget = Some(budget.map_or(limit, |previous: usize| previous.min(limit)));
        } else {
            return budget;
        }
        if next.after != "|" {
            break;
        }
    }
    budget
}

/// Return actionable correction, without rewriting the command or changing status semantics.
pub(super) fn check(command: &str) -> Option<String> {
    // The existing safety guard still checks oversized/unclear shell programs.
    // Bound this additional best-effort classifier's allocations and latency.
    if command.len() > 256 * 1024 {
        return None;
    }
    let segments = shell_segments(command);
    let mut total = 0usize;
    for (index, segment) in segments.iter().enumerate() {
        let Some(words) = argv(segment.text) else {
            continue;
        };
        let Some(search) = search_args(&words) else {
            continue;
        };
        let mut pipeline_end = index;
        while segments[pipeline_end].after == "|" && pipeline_end + 1 < segments.len() {
            pipeline_end += 1;
        }
        if segments[pipeline_end].after == "||"
            && segments
                .get(pipeline_end + 1)
                .and_then(|next| argv(next.text))
                .is_some_and(|args| args == ["true"] || args == [":"])
        {
            return Some(format!(
                "BLOCKED: blanket search-error suppression. {OPTIONAL_STATUS}"
            ));
        }
        if search.quiet {
            continue;
        }
        let broad = search.paths.is_empty()
            || search.paths.len() > 3
            || search.paths.iter().any(|path| !explicit_file(path));
        if broad && graph_inventory(&search.patterns) {
            return Some(format!("BLOCKED: structural/MCP discovery across directories. Use graph MCP (AST, callers and dependents) first, then inspect specific files or symbol slices. {OPTIONAL_STATUS}"));
        }
        let budget = output_budget(&segments, index, &words);
        if broad && budget.is_none() {
            return Some(format!("BLOCKED: unbounded multi-file search output. Narrow to specific files; cap exploration to at most {MAX_LINES} lines total (for example sed -n '1,40p'), then summarize. rg -m limits matches per file, not total output. {OPTIONAL_STATUS}"));
        }
        if let Some(lines) = budget {
            total = total.saturating_add(lines);
            if total > MAX_LINES {
                return Some(format!("BLOCKED: exploration output budget exceeds {MAX_LINES} lines total. Split into targeted graph/file queries and summarize results. {OPTIONAL_STATUS}"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reported_script_is_blocked_with_graph_correction() {
        let script = "set -euo pipefail\nprintf '%s\\n' '--- imports and decorators ---'\nrg -n \"^(from|import)|@triton\\.jit|def ([a-zA-Z_0-9]+)\" research/disk_codec --glob '*.py' | sed -n '1,260p'\nprintf '%s\\n' '--- external call sites ---'\nrg -n disk_codec . --glob '!research/disk_codec/**' | sed -n '1,240p'\nrg -n 'ast_context_tool|query_graph_tool|get_minimal_context_tool|semantic_search_nodes_tool' .venv/bin .venv/lib/python3.12/site-packages --glob '*.py' -l | sed -n '1,80p'";
        assert!(check(script).unwrap().contains("graph MCP"));
        assert!(
            check("rg -l 'ast_context_tool' .venv/bin .venv/lib | sed -n '1,80p'")
                .unwrap()
                .contains("graph MCP")
        );
    }

    #[test]
    fn bounded_targeted_searches_and_summary_checks_are_allowed() {
        for command in [
            "rg -n symbol src/codec.py",
            "rg -n symbol src/codec.py src/core.py",
            "rg -n symbol src | sed -n '1,40p'",
            "rg --files src | head -n 80",
            "rg -n symbol src | wc -l",
            "rg -n symbol src > /tmp/search.log",
            "rg -n symbol src 2>/tmp/errors.log | sed -n '1,40p'",
            "rg --regexp=symbol --glob=*.py src | sed -n '1,40p'",
            "rg -n symbol src |\nsed -n '1,40p'",
            "rg -n café src/codéc.py",
            "if rg -q symbol src; then echo found; fi",
            "rg -n symbol src/codec.py || { rg_status=$?; [ \"$rg_status\" -eq 1 ] || exit \"$rg_status\"; }",
            "rg -n symbol src | sed -n '1,40p'\nrg --files tests | sed -n '1,40p'",
        ] { assert!(check(command).is_none(), "{command}: {:?}", check(command)); }
    }

    #[test]
    fn rejects_unbounded_or_over_budget_discovery() {
        for command in [
            "rg -n term src",
            "grep -Rn term src",
            "rg --files src",
            "rg -m 1 term src",
            "rg -l term src",
            "rg -n term . | sed -n '1,81p'",
            "rg term src | head -n 50\nrg other tests | head -n 40",
            "printf heading\nrg -n term src",
            "rg -n term src 2>/tmp/errors.log",
            "rg -n term src 2> /tmp/errors.log",
        ] {
            assert!(check(command).is_some(), "{command}");
        }
    }

    #[test]
    fn quoted_data_comments_and_heredocs_are_not_commands() {
        for command in [
            "echo 'rg -n term src | head -n 900'",
            "printf '%s\\n' 'rg' '-n' 'term' 'src'",
            "# rg -n term src\nprintf hello",
            "cat <<'EOF'\nrg -n term src\nEOF\nprintf done",
            "cat <<-EOF\n\trg -n term src\n\tEOF\nprintf done",
            "cat <<'END DATA'\nrg -n term src\nEND DATA\nprintf done",
            "cat <<EOF\nrg term src\nEOF\ncat <<TWO\nrg term src\nTWO",
            "python -c 'print(\"rg -n term src\")'",
            "rg 'a|b;import' src/codec.py",
        ] {
            assert!(check(command).is_none(), "{command}: {:?}", check(command));
        }
        assert!(check("cat <<'EOF'\nrg term src\nEOF\nrg actual src").is_some());
    }

    #[test]
    fn preserves_deliberate_exit_status_semantics() {
        assert!(check("rg term src/codec.py || true")
            .unwrap()
            .contains("-eq 1"));
        assert!(check("rg term src/codec.py || :").is_some());
        assert!(check("rg term src | sed -n '1,40p' || true").is_some());
        for command in [
            "rg -q term src && echo found",
            "! rg -q term src",
            "if rg term src/codec.py; then echo found; fi",
            "set -euo pipefail\nrg required src/codec.py",
            "rg term src/codec.py || exit $?",
            "rg term src/codec.py || { code=$?; [ \"$code\" -eq 1 ] || exit \"$code\"; }",
        ] {
            assert!(check(command).is_none(), "{command}");
        }
    }

    #[test]
    fn classifier_is_bounded_and_linear_fixture() {
        let fixture = "printf '%s\\n' 'example rg term src'\n".repeat(1000);
        let start = std::time::Instant::now();
        for _ in 0..100 {
            assert!(check(&fixture).is_none());
        }
        eprintln!(
            "inspection guard: 100 x {} byte fixture: {:?}",
            fixture.len(),
            start.elapsed()
        );
        assert!(check(&"x".repeat(256 * 1024 + 1)).is_none());
    }
}
