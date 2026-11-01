//! Canonical generated command ownership. Arbitrary shell snippets, unknown
//! options and commands merely mentioning Forge or a Python module are preserved.
use crate::provider::Provider;
use conductor_native::hook_installer::{shlex_join, shlex_split};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Owned {
    Native,
    Legacy,
}

fn pairs(tokens: &[String], allowed: &[&str], switches: &[&str]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    let mut i = 0;
    while i < tokens.len() {
        if !seen.insert(&tokens[i]) {
            return false;
        }
        if switches.contains(&tokens[i].as_str()) {
            i += 1;
        } else if allowed.contains(&tokens[i].as_str()) && i + 1 < tokens.len() {
            i += 2;
        } else {
            return false;
        }
    }
    true
}

fn flag<'a>(tokens: &'a [String], key: &str) -> Option<&'a str> {
    tokens
        .windows(2)
        .find(|pair| pair[0] == key)
        .map(|pair| pair[1].as_str())
}

fn env_prefix(tokens: &[String]) -> Option<usize> {
    // Historical project-init emitted `env CONDUCTOR_PYTHON=... forge ...`.
    // Accept its exact launcher, not env options or arbitrary injected variables.
    let mut at = usize::from(tokens.first().is_some_and(|token| token == "env"));
    let mut seen = std::collections::BTreeSet::new();
    while let Some(token) = tokens.get(at) {
        let Some((key, value)) = token.split_once('=') else {
            break;
        };
        if !matches!(
            key,
            "FORGE_MODE"
                | "FORGE_HOOK_STANDALONE"
                | "FORGE_PYTHON"
                | "FORGE_PYTHONPATH"
                | "CONDUCTOR_PYTHON"
                | "A2A_AGENT_NAME"
        ) || value.is_empty()
            || !seen.insert(key)
        {
            return None;
        }
        at += 1;
    }
    Some(at)
}

fn native(tokens: &[String], provider: Provider) -> bool {
    if tokens.len() >= 3 && tokens[1] == "hook" && provider.full_hooks() {
        return crate::hooks_install::EVENTS.contains(&tokens[2].as_str())
            && pairs(
                &tokens[3..],
                &["--protocol", "--host", "--project-dir"],
                &[],
            )
            && flag(&tokens[3..], "--protocol").is_none_or(|p| p == provider.name());
    }
    if tokens.get(1).map(String::as_str) != Some("mailbox") {
        return false;
    }
    let args = if tokens.get(2).map(String::as_str) == Some("--host") {
        tokens.get(4..).unwrap_or_default()
    } else {
        &tokens[2..]
    };
    args.first().map(String::as_str) == Some("startup-hook")
        && flag(&args[1..], "--provider") == Some(provider.name())
        && pairs(
            &args[1..],
            &[
                "--provider",
                "--host",
                "--state-dir",
                "--identity",
                "--serve-timeout-seconds",
                "--lifetime-seconds",
            ],
            &["--once-per-session"],
        )
}

fn legacy(tokens: &[String], provider: Provider) -> bool {
    let Some(program) = tokens
        .first()
        .and_then(|p| Path::new(p).file_name())
        .and_then(|p| p.to_str())
    else {
        return false;
    };
    let python = program == "python"
        || program == "python3"
        || program
            .strip_prefix("python3.")
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()));
    if !python || tokens.get(1).map(String::as_str) != Some("-m") {
        return false;
    }
    match tokens.get(2).map(String::as_str) {
        Some("conductor.a2a_session_start") => {
            let args = &tokens[3..];
            flag(args, "--provider").unwrap_or("codex") == provider.name()
                && pairs(
                    args,
                    &[
                        "--provider",
                        "--output",
                        "--max-messages",
                        "--preview-chars",
                        "--max-chars",
                        "--identity",
                        "--state-dir",
                        "--serve-timeout",
                    ],
                    &["--once-per-session"],
                )
        }
        Some("tooling.hooks.dispatch") if provider.full_hooks() => {
            tokens
                .get(3)
                .is_some_and(|event| crate::hooks_install::EVENTS.contains(&event.as_str()))
                && pairs(&tokens[4..], &["--project-dir", "--protocol"], &[])
                && flag(&tokens[4..], "--protocol").unwrap_or("claude") == provider.name()
        }
        _ => false,
    }
}

pub(super) fn owned(value: &Value, provider: Provider) -> Option<Owned> {
    if value["type"] != "command" {
        return None;
    }
    let command = value["command"].as_str()?;
    // The historical Claude launcher uses a deliberate shell variable. Accept
    // only this exact generated shape, never a prefix or chained command.
    if provider == Provider::Claude
        && crate::hooks_install::EVENTS.iter().any(|event| {
            command == format!("$CLAUDE_PROJECT_DIR/.claude/hooks/dispatch.py {event}")
        })
    {
        return Some(Owned::Legacy);
    }
    let tokens = shlex_split(command).ok()?;
    if shlex_join(&tokens) != command {
        return None;
    }
    let args = tokens.get(env_prefix(&tokens)?..)?;
    let program = args
        .first()
        .and_then(|p| Path::new(p).file_name())?
        .to_str()?;
    if program == "forge" && native(args, provider) {
        Some(Owned::Native)
    } else if legacy(args, provider) {
        Some(Owned::Legacy)
    } else {
        None
    }
}
