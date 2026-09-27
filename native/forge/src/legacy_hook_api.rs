//! Compatibility API for installed Python hook bodies. The computation lives
//! in the same native modules used by `forge hook`; Python only forwards JSON.

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::{bash_guard, bash_impact, crg_gate, read_budget, tool_quiet, write_targets};

fn field<'a>(request: &'a Value, name: &str) -> Result<&'a str> {
    request
        .get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("legacy hook request needs string field {name:?}"))
}

fn value<'a>(request: &'a Value, name: &str) -> Result<&'a Value> {
    request
        .get(name)
        .with_context(|| format!("legacy hook request needs field {name:?}"))
}

fn quiet_config<'a>(
    request: &'a Value,
    save_dir: &'a Path,
    repo_root: &'a Path,
) -> Result<tool_quiet::QuietConfig<'a>> {
    Ok(tool_quiet::QuietConfig {
        save_dir,
        repo_root,
        now_stamp: field(request, "stamp")?,
        output_field: field(request, "output_field")?,
    })
}

fn quiet_result(request: &Value, bash: bool, envelope: bool) -> Result<Value> {
    let save_dir = Path::new(field(request, "save_dir")?);
    let repo_root = Path::new(field(request, "repo_root")?);
    let config = quiet_config(request, save_dir, repo_root)?;
    let response = value(request, "payload")?;
    let raw_limit = request
        .get("limit_bytes")
        .and_then(Value::as_i64)
        .with_context(|| "legacy quiet request needs integer limit_bytes")?;
    let disabled = raw_limit <= 0
        || request
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let limit = raw_limit.max(0) as usize;
    if bash {
        if disabled {
            return Ok(if envelope {
                json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
            } else {
                Value::Null
            });
        }
        return Ok(if envelope {
            tool_quiet::rewrite_envelope_bash(response, limit, &config)
        } else {
            tool_quiet::bound_bash_response(response, limit, &config).unwrap_or(Value::Null)
        });
    }
    if envelope {
        let (answer, warning) =
            tool_quiet::rewrite_envelope_tool(response, limit, disabled, &config);
        if let Some(warning) = warning {
            eprintln!("post-tool-quiet: unrecognized tool_response shape ({warning}); passing through unbounded");
        }
        return Ok(answer);
    }
    let outcome = tool_quiet::bound_tool_response(response, limit, disabled, &config);
    Ok(match outcome {
        tool_quiet::ToolOutcome::NoChange => Value::Null,
        tool_quiet::ToolOutcome::Updated(answer) => answer,
        tool_quiet::ToolOutcome::Unrecognized(kind) => {
            eprintln!("post-tool-quiet: unrecognized tool_response shape ({kind}); passing through unbounded");
            Value::Null
        }
    })
}

pub fn evaluate(operation: &str, request: &Value) -> Result<Value> {
    match operation {
        "write-targets" => Ok(json!(write_targets::write_targets(field(
            request, "command"
        )?))),
        "repo-write-targets" => Ok(json!(write_targets::repo_write_targets(
            field(request, "command")?,
            Path::new(field(request, "repo_root")?),
        ))),
        "working-directory" => Ok(json!(write_targets::working_directory(
            field(request, "command")?,
            Path::new(field(request, "repo_root")?),
        )
        .map(|path| path.to_string_lossy().into_owned()))),
        "split-commands" => {
            let tokens: Vec<String> = serde_json::from_value(value(request, "tokens")?.clone())
                .context("legacy split-commands expects string tokens")?;
            Ok(json!(write_targets::split_commands(&tokens)))
        }
        "guard-check" => Ok(json!(bash_guard::check(field(request, "command")?))),
        "guard-check-command" => {
            let argv: Vec<String> = serde_json::from_value(value(request, "argv")?.clone())
                .context("legacy guard-check-command expects string argv")?;
            Ok(json!(bash_guard::check_command(&argv)))
        }
        "impact-classify" => {
            let (tier, detail) = bash_impact::classify(field(request, "command")?);
            let tier = match tier {
                bash_impact::Tier::Allow => "allow",
                bash_impact::Tier::SoftWarn => "soft_warn",
            };
            Ok(json!([tier, detail]))
        }
        "impact-context" => Ok(json!(bash_impact::additional_context(field(
            request, "command"
        )?))),
        "bash-quiet-envelope" => quiet_result(request, true, true),
        "bash-quiet-response" => quiet_result(request, true, false),
        "tool-quiet-envelope" => quiet_result(request, false, true),
        "tool-quiet-response" => quiet_result(request, false, false),
        "read-budget" => read_budget::hook_output(
            value(request, "payload")?,
            Path::new(field(request, "state_dir")?),
        ),
        "read-response-chars" => {
            let limit = request
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(4_000_000);
            Ok(json!(read_budget::response_chars(
                value(request, "payload")?,
                limit as usize
            )))
        }
        "read-tally" => {
            let tokens = value(request, "tokens")?
                .as_u64()
                .context("legacy read-tally expects nonnegative integer tokens")?;
            let (previous, total) = read_budget::tally(
                Path::new(field(request, "state_dir")?),
                field(request, "key")?,
                tokens,
            )?;
            Ok(json!([previous, total]))
        }
        "gate-start" => {
            crg_gate::start(value(request, "payload")?)?;
            Ok(Value::Null)
        }
        "gate-verify-bash" => {
            let repo_root = Path::new(field(request, "repo_root")?);
            let common_dir = crg_gate::checkout_of(repo_root).map(|(_root, common)| common);
            let env: HashMap<String, String> = std::env::vars().collect();
            Ok(crg_gate::verify_bash(
                value(request, "payload")?,
                field(request, "owner")?,
                repo_root,
                common_dir.as_deref(),
                &env,
            ))
        }
        other => bail!("unknown legacy hook operation {other:?}"),
    }
}

pub fn run(operation: &str) -> Result<u8> {
    const MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;
    let mut raw = String::new();
    io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_string(&mut raw)
        .context("cannot read legacy hook request")?;
    if raw.len() as u64 > MAX_REQUEST_BYTES {
        bail!("legacy hook JSON request exceeds {MAX_REQUEST_BYTES} bytes");
    }
    let request: Value = serde_json::from_str(&raw).context("invalid legacy hook JSON request")?;
    let answer = evaluate(operation, &request)?;
    println!("{}", serde_json::to_string(&answer)?);
    Ok(0)
}
