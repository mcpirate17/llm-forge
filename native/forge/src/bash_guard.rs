//! Native port of `tooling/hooks/claude/_bash_guard.py`: command-position deny
//! rules for the PreToolUse/Bash hook.
//!
//! Matching happens at COMMAND POSITION -- the command is tokenized with the
//! same posix-shlex state machine as `write_targets` (quoted spans collapse
//! into single tokens and can no longer look like commands), split on shell
//! operators, and each resulting command is matched by its own argv. If
//! tokenization fails, this falls back to the original anywhere-in-string
//! regexes: for a deny hook, over-blocking is the safe failure.
use std::sync::LazyLock;

use regex::Regex;

use crate::write_targets::{posix_shlex, split_commands};

const SHELL_RUNNERS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh"];
const RECURSIVE_FLAGS: &[&str] = &["-r", "-R", "--recursive"];

const REASON_PUSH_FORCE: &str =
    "BLOCKED: git push --force. Use --force-with-lease if you must, or ask the user.";
const REASON_RESET_HARD: &str =
    "BLOCKED: git reset --hard destroys uncommitted work. Stash or commit first.";
const REASON_GIT_CLEAN: &str = "BLOCKED: git clean deletes untracked files permanently. \
     Be specific about what to remove.";
const REASON_RM_DANGER: &str = "BLOCKED: Dangerous recursive delete target.";
const REASON_PIP: &str = "BLOCKED: Use 'uv pip install' instead of raw pip.";

struct FallbackPattern {
    regex: &'static LazyLock<Regex>,
    label: &'static str,
}

static GIT_PUSH_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"git\s+push\s+").unwrap());
static FORCE_FLAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:-f|--force)\b").unwrap());
static FALLBACK_RESET_HARD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"git\s+reset\s+--hard\b").unwrap());
static FALLBACK_GIT_CLEAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"git\s+clean\s+-[fdxX]").unwrap());
static FALLBACK_RM_DANGER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"rm\s+-r[f ]*\s+(/|~/|\.\./|/home)\b").unwrap());
static FALLBACK_PIP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*pip\s+install\b").unwrap());
static FALLBACK_PYTHON_PIP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*python.*-m\s+pip\s+install\b").unwrap());
// Python's fallback pattern is `git\s+push\s+.*(?<!-with-lease)(-f|--force)\b`.
// `regex` has no lookbehind, so it's emulated: find "git push", then scan
// `(-f|--force)\b` occurrences right-to-left (mirroring the greedy `.*`,
// which prefers the rightmost split) and take the first whose preceding 11
// bytes aren't "-with-lease". Note the lookbehind checks text BEFORE the
// flag, not after -- for "--force-with-lease" the excluded text follows the
// match, so the lookbehind does not exclude it and this (like Python) still
// blocks it on the unparseable-command path. That is a latent quirk of the
// original fallback regex, preserved here rather than fixed, since only the
// tokenized path (`has_force`) is documented/tested as force-with-lease-safe.
fn push_force_fallback_matches(command: &str) -> bool {
    let Some(prefix) = GIT_PUSH_PREFIX.find(command) else {
        return false;
    };
    let mut candidates: Vec<_> = FORCE_FLAG
        .find_iter(command)
        .filter(|m| m.start() >= prefix.end())
        .collect();
    candidates.reverse();
    for m in candidates {
        let start = m.start();
        let behind = start.saturating_sub(11);
        if !command.is_char_boundary(behind) {
            return true;
        }
        if &command[behind..start] != "-with-lease" {
            return true;
        }
    }
    false
}

static FALLBACK_PATTERNS: LazyLock<Vec<FallbackPattern>> = LazyLock::new(|| {
    vec![
        FallbackPattern {
            regex: &FALLBACK_RESET_HARD,
            label: "git reset --hard",
        },
        FallbackPattern {
            regex: &FALLBACK_GIT_CLEAN,
            label: "git clean -fd",
        },
        FallbackPattern {
            regex: &FALLBACK_RM_DANGER,
            label: "recursive delete of a dangerous target",
        },
        FallbackPattern {
            regex: &FALLBACK_PIP,
            label: "raw pip install",
        },
        FallbackPattern {
            regex: &FALLBACK_PYTHON_PIP,
            label: "raw python -m pip install",
        },
    ]
});

fn basename(exe: &str) -> &str {
    exe.rsplit('/').next().unwrap_or(exe)
}

/// True for `-f`/`--force`, but NOT `--force-with-lease`.
fn has_force(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "-f" || (a.starts_with("--force") && a != "--force-with-lease"))
}

/// Return a block reason for one command's argv, or `None` to allow.
fn check_command(argv: &[String]) -> Option<String> {
    if argv.is_empty() {
        return None;
    }
    let exe = basename(&argv[0]).to_string();
    let args = &argv[1..];

    // bash -c "<payload>" -- re-check the payload as its own command line.
    if SHELL_RUNNERS.contains(&exe.as_str()) {
        if let Some(index) = args.iter().position(|a| a == "-c") {
            if let Some(payload) = args.get(index + 1) {
                return check(payload);
            }
        }
    }

    if exe == "git" && !args.is_empty() {
        let sub = args[0].as_str();
        let rest = &args[1..];
        if sub == "push" && has_force(rest) {
            return Some(REASON_PUSH_FORCE.to_string());
        }
        if sub == "reset" && rest.iter().any(|a| a == "--hard") {
            return Some(REASON_RESET_HARD.to_string());
        }
        // Only SHORT flags are char-tested: "--dry-run" contains a "d" and
        // must not be mistaken for the destructive -d.
        let clean_hit = rest.iter().any(|a| {
            (a.starts_with("--") && a == "--force")
                || (a.starts_with('-')
                    && !a.starts_with("--")
                    && a[1..].chars().any(|c| "fdxX".contains(c)))
        });
        if sub == "clean" && clean_hit {
            return Some(REASON_GIT_CLEAN.to_string());
        }
    }

    if exe == "rm"
        && args.iter().any(|a| {
            RECURSIVE_FLAGS.contains(&a.as_str())
                || (a.starts_with('-') && !a.starts_with("--") && a.to_lowercase().contains('r'))
        })
    {
        for arg in args {
            if arg.starts_with('-') {
                continue;
            }
            if arg == "/" || arg.starts_with('/') || arg.starts_with("~/") || arg.starts_with("../")
            {
                return Some(REASON_RM_DANGER.to_string());
            }
        }
    }

    if exe == "pip" && args.first().map(|a| a == "install").unwrap_or(false) {
        return Some(REASON_PIP.to_string());
    }

    if exe.starts_with("python") {
        if let Some(index) = args.iter().position(|a| a == "-m") {
            if args.get(index + 1).map(|s| s.as_str()) == Some("pip")
                && args.get(index + 2).map(|s| s.as_str()) == Some("install")
            {
                return Some(REASON_PIP.to_string());
            }
        }
    }

    None
}

/// Return a block reason for a full command line, or `None` to allow.
pub fn check(command: &str) -> Option<String> {
    let tokens = match posix_shlex::tokenize(command, posix_shlex::PUNCTUATION_CHARS_FULL) {
        Ok(tokens) => tokens,
        Err(_) => {
            // Unparseable: fall back to the permissive-parse / aggressive-match path.
            if push_force_fallback_matches(command) {
                return Some(
                    "BLOCKED (git push --force): command could not be parsed, matched conservatively."
                        .to_string(),
                );
            }
            for pattern in FALLBACK_PATTERNS.iter() {
                if pattern.regex.is_match(command) {
                    return Some(format!(
                        "BLOCKED ({}): command could not be parsed, matched conservatively.",
                        pattern.label
                    ));
                }
            }
            return None;
        }
    };

    for argv in split_commands(&tokens) {
        if let Some(reason) = check_command(&argv) {
            return Some(reason);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(command: &str) -> bool {
        check(command).is_some()
    }

    #[test]
    fn blocks_force_push_but_not_force_with_lease() {
        assert!(blocked("git push --force origin master"));
        assert!(blocked("git push -f origin master"));
        assert!(!blocked("git push --force-with-lease origin master"));
        assert!(!blocked("git push origin master"));
    }

    #[test]
    fn blocks_reset_hard_but_not_soft() {
        assert!(blocked("git reset --hard HEAD~1"));
        assert!(blocked("cd /tmp && git reset --hard"));
        assert!(!blocked("git reset --soft HEAD~1"));
    }

    #[test]
    fn blocks_git_clean_short_flags_but_not_dry_run() {
        assert!(blocked("git clean -fd"));
        assert!(blocked("git clean -fdx"));
        assert!(blocked("git clean --force"));
        assert!(!blocked("git clean --dry-run"));
    }

    #[test]
    fn blocks_recursive_rm_of_dangerous_targets_only() {
        assert!(blocked("rm -rf /"));
        assert!(blocked("rm -rf /home/tim/stuff"));
        assert!(blocked("rm -rf ~/important"));
        assert!(blocked("rm -r ../sibling"));
        assert!(!blocked("rm -rf ./build"));
        assert!(!blocked("rm -rf research/tmp/scratch"));
        assert!(!blocked("rm -f /tmp/single_file.txt"));
    }

    #[test]
    fn blocks_raw_pip_but_not_uv_or_other_modules() {
        assert!(blocked("pip install numpy"));
        assert!(blocked("python -m pip install numpy"));
        assert!(blocked("python3 -m pip install numpy"));
        assert!(!blocked("uv pip install numpy"));
        assert!(!blocked(
            "python3 -m research.tools.rotate_current_work --apply"
        ));
    }

    #[test]
    fn recurses_into_shell_runner_payloads() {
        assert!(blocked(r#"bash -c "git push --force""#));
        assert!(blocked("sh -c 'git reset --hard'"));
        assert!(!blocked(r#"bash -c "ls -la""#));
    }

    #[test]
    fn splits_on_shell_operators() {
        assert!(blocked("ls; git reset --hard"));
        assert!(blocked("true && rm -rf /etc"));
        assert!(blocked("echo a || git push --force origin x"));
        assert!(!blocked("ls -la && echo done"));
    }

    #[test]
    fn quoted_mentions_are_not_commands() {
        assert!(!blocked(
            r#"echo '{"tool_input":{"command":"git push --force origin master"}}' | ./hook.sh"#
        ));
        assert!(!blocked(r#"echo "git reset --hard is blocked""#));
        assert!(!blocked(r#"grep -rn "git clean -fd" docs/"#));
    }

    #[test]
    fn unparseable_command_falls_back_to_conservative_patterns() {
        assert!(blocked(r#"git push --force origin master "oops"#));
        assert!(blocked(r#"git reset --hard "oops"#));
        assert!(!blocked(r#"echo "hello"#));
    }
}
