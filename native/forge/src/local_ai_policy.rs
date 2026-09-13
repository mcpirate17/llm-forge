//! Native port of `conductor.local_ai_policy`: fail-closed authority boundary
//! for local generative models. Local models are clerical (notes, summaries,
//! organization, compaction) and never approve or authorize work; enforcement
//! lives on the *prompt* side (`prompt_requests_authority`) and, for a shell
//! command that looks like a local-chat invocation, on the command side
//! (`deny_local_ai_command`). Ported for `current_work_guard_bash`, which
//! calls it on every Bash command.

use std::path::Path;
use std::sync::LazyLock;

use regex::{Regex, RegexBuilder};

use crate::write_targets::posix_shlex::{self, PUNCTUATION_CHARS_NARROW};

pub const DENY_REASON: &str =
    "BLOCKED: local AI is clerical-only and has zero approval authority. Local \
models may handle notes, summaries, organization, or compaction, but may never \
approve, authorize, sign off, promote, launch, resume, or continue work or \
runs. Use Tim or a runtime-verified frontier model where policy permits; \
multi-hour training still requires Tim's explicit approval.";

pub const UNCLASSIFIED_REASON: &str =
    "BLOCKED: local generative inference requires an explicit clerical task class. \
Set LOCAL_AI_TASK to notes, summary, organization, or compaction. Local AI \
cannot be used for approval or run decisions.";

const ALLOWED_LOCAL_TASKS: [&str; 4] = ["notes", "summary", "organization", "compaction"];
const SHELL_OPERATORS: [&str; 5] = [";", "&", "&&", "|", "||"];
const SHELL_RUNNERS: [&str; 5] = ["bash", "dash", "ksh", "sh", "zsh"];

static LOCAL_CHAT_ENDPOINT_RE: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(
        r"https?://(?:127\.0\.0\.1|localhost|\[::1\])(?::11434)?/api/(?:chat|generate)\b",
    )
    .case_insensitive(true)
    .build()
    .expect("valid regex")
});

static TASK_ASSIGNMENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s;])(?:export\s+)?LOCAL_AI_TASK=([A-Za-z_-]+)(?:$|[\s;])")
        .expect("valid regex")
});

static AUTHORITY_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\bapprov(?:e|al|ed|er|ing)\b",
        r"\bauthori[sz](?:e|ed|ation|ing)\b",
        r"\bpermission\b",
        r"\bsign(?:ed|ing)?[- ]?off\b",
        r"\bgreen[- ]?light\b",
        r"\bgo\s*[/_-]?\s*no[- ]?go\b",
        r"\bfinal\s+verdict\b",
        r"\b(?:should|may|can)\s+(?:i|we|the\s+agent)\b",
        r"\b(?:decide|recommend|determine)\s+(?:whether|if)\b",
        r"\breturn\b.{0,40}\b(?:pass|not[_ -]?ready|fail[- ]?closed)\b",
        r"\b(?:launch|relaunch|resume|continue|start|conduct|promote)\w*\b.{0,50}\b(?:run|training|experiment|optimizer|smoke)\w*\b",
        r"\b(?:run|training|experiment|optimizer|smoke)\w*\b.{0,50}\b(?:launch|relaunch|resume|continue|start|conduct|promote)\w*\b",
    ]
    .iter()
    .map(|pattern| {
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .dot_matches_new_line(true)
            .build()
            .expect("valid regex")
    })
    .collect()
});

pub fn prompt_requests_authority(prompt: &str) -> bool {
    AUTHORITY_PATTERNS
        .iter()
        .any(|pattern| pattern.is_match(prompt))
}

/// `require_clerical_task`: `Ok(normalized_class)` or `Err(reason)` -- the
/// reason is either `DENY_REASON` (authority request) or a class-invalid
/// message mirroring `LocalAIPolicyError`'s text (not itself matched against
/// by any caller, so an approximate wording is safe).
fn require_clerical_task(task_class: &str, prompt: &str) -> Result<String, String> {
    let normalized = task_class.trim().to_lowercase().replace('-', "_");
    if !ALLOWED_LOCAL_TASKS.contains(&normalized.as_str()) {
        let mut expected: Vec<&str> = ALLOWED_LOCAL_TASKS.to_vec();
        expected.sort_unstable();
        return Err(format!(
            "local task class {task_class:?} is not clerical; expected one of {}",
            expected.join(", ")
        ));
    }
    if prompt_requests_authority(prompt) {
        return Err(DENY_REASON.to_string());
    }
    Ok(normalized)
}

fn basename(token: &str) -> String {
    Path::new(token)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn command_segments(command: &str) -> Option<Vec<Vec<String>>> {
    let tokens = posix_shlex::tokenize_no_comments(command, PUNCTUATION_CHARS_NARROW).ok()?;
    let mut segments: Vec<Vec<String>> = vec![Vec::new()];
    for token in tokens {
        if SHELL_OPERATORS.contains(&token.as_str()) {
            if !segments.last().unwrap().is_empty() {
                segments.push(Vec::new());
            }
            continue;
        }
        segments.last_mut().unwrap().push(token);
    }
    Some(segments.into_iter().filter(|s| !s.is_empty()).collect())
}

fn is_assignment(token: &str) -> bool {
    match token.find('=') {
        Some(index) if index > 0 => {
            let name = &token[..index];
            let mut chars = name.chars();
            let first = chars.next().unwrap();
            (first.is_ascii_alphabetic() || first == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

fn command_argv(segment: &[String]) -> (String, Vec<String>) {
    let mut index = 0usize;
    while index < segment.len() && is_assignment(&segment[index]) {
        index += 1;
    }
    if index < segment.len() && basename(&segment[index]) == "env" {
        index += 1;
        while index < segment.len() {
            let token = &segment[index];
            if token.starts_with('-') || is_assignment(token) {
                index += 1;
                continue;
            }
            break;
        }
    }
    if index >= segment.len() {
        return (String::new(), Vec::new());
    }
    (basename(&segment[index]), segment[index + 1..].to_vec())
}

fn is_local_chat(segment: &[String]) -> bool {
    let (executable, arguments) = command_argv(segment);
    if SHELL_RUNNERS.contains(&executable.as_str()) {
        if let Some(index) = arguments.iter().position(|a| a == "-c") {
            if let Some(inline) = arguments.get(index + 1) {
                return deny_local_ai_command(inline, false).is_some();
            }
        }
    }
    if executable == "ollama" && arguments.first().map(String::as_str) == Some("run") {
        return true;
    }
    !matches!(executable.as_str(), "echo" | "printf" | "rg" | "grep")
        && segment
            .iter()
            .any(|token| LOCAL_CHAT_ENDPOINT_RE.is_match(token))
}

fn task_class(command: &str) -> Option<String> {
    TASK_ASSIGNMENT_RE
        .captures(command)
        .map(|caps| caps[1].to_string())
}

/// `deny_local_ai_command`: a hook denial for unsafe local inference, or
/// `None` when the command is fine.
pub fn deny_local_ai_command(command: &str, local_runtime: bool) -> Option<String> {
    if local_runtime && prompt_requests_authority(command) {
        return Some(DENY_REASON.to_string());
    }
    let Some(segments) = command_segments(command) else {
        let ollama_run = Regex::new(r"\bollama\s+run\b").unwrap().is_match(command);
        if ollama_run || LOCAL_CHAT_ENDPOINT_RE.is_match(command) {
            return Some(UNCLASSIFIED_REASON.to_string());
        }
        return None;
    };
    let local_segments: Vec<&Vec<String>> = segments.iter().filter(|s| is_local_chat(s)).collect();
    if local_segments.is_empty() {
        return None;
    }
    let Some(class) = task_class(command) else {
        return Some(UNCLASSIFIED_REASON.to_string());
    };
    require_clerical_task(&class, command).err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_command_is_allowed() {
        assert_eq!(deny_local_ai_command("ls -la", false), None);
    }

    #[test]
    fn ollama_run_without_task_is_unclassified() {
        assert_eq!(
            deny_local_ai_command("ollama run qwen3", false),
            Some(UNCLASSIFIED_REASON.to_string())
        );
    }

    #[test]
    fn ollama_run_with_clerical_task_is_allowed() {
        assert_eq!(
            deny_local_ai_command("LOCAL_AI_TASK=notes ollama run qwen3", false),
            None
        );
    }

    #[test]
    fn ollama_run_with_non_clerical_task_denies() {
        let reason = deny_local_ai_command("LOCAL_AI_TASK=approval ollama run qwen3", false);
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("not clerical"));
    }

    #[test]
    fn authority_request_over_curl_denies() {
        let cmd =
            "LOCAL_AI_TASK=notes curl http://127.0.0.1:11434/api/chat should I approve this launch";
        let reason = deny_local_ai_command(cmd, false);
        assert_eq!(reason, Some(DENY_REASON.to_string()));
    }

    #[test]
    fn local_runtime_prompt_authority_denies_directly() {
        assert_eq!(
            deny_local_ai_command("please approve this launch", true),
            Some(DENY_REASON.to_string())
        );
    }

    #[test]
    fn hash_is_an_ordinary_word_character_not_a_comment() {
        // commenters="" in the Python lexer: a bare `#` command must still
        // tokenize (empty segments), not silently truncate the command.
        assert_eq!(deny_local_ai_command("echo hi #not a comment", false), None);
    }
}
