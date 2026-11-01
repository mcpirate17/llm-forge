use super::{cell, dispatch};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub(super) fn active_state(root: &Path) -> Value {
    if std::fs::symlink_metadata(root.join("conductor/active_state.json"))
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return cell(
            "active-state-live-claims",
            "NOT_READY",
            "active state cache not present",
            json!({}),
        );
    }
    let result = (|| -> anyhow::Result<Value> {
        let path = root.join("conductor/active_state.json");
        let bytes = forge::bounded_file::read(&path, 1024 * 1024)?;
        let state: forge::active_state::ActiveState = serde_json::from_slice(&bytes)?;
        forge::active_state::validate_active_state(&state, forge::instant::now())
            .map_err(anyhow::Error::msg)?;
        let (claims, digest) = forge::ownership::load_claims(root)?;
        let now = forge::instant::now();
        let mut live = Vec::new();
        for claim in claims {
            if claim.active(now)? {
                live.push(claim.claim_id);
            }
        }
        live.sort();
        let mut cached: Vec<_> = state
            .active_claims
            .iter()
            .map(|claim| claim.claim_id.clone())
            .collect();
        cached.sort();
        let age = now
            - forge::instant::parse(&state.last_updated)
                .ok_or_else(|| anyhow::anyhow!("invalid state timestamp"))?;
        dispatch(
            "check_active_state",
            &json!({"live_ids":live,"cached_ids":cached,"age_seconds":age,
            "active_state_sha256":format!("{:x}", Sha256::digest(bytes)),"claim_store_sha256":digest}),
        )
    })();
    result.unwrap_or_else(|error| {
        cell(
            "active-state-live-claims",
            "FAIL-CLOSED",
            &format!("state or live claim source unavailable: {error:#}"),
            json!({}),
        )
    })
}

fn probe(command: &mut Command, seconds: u64) -> anyhow::Result<std::process::Output> {
    forge::process_probe::run(command, Duration::from_secs(seconds), 256 * 1024)
}
pub(super) fn providers(root: &Path, binary: &Path, mode: &str, identity: Option<&str>) -> Value {
    let mut evidence = Map::new();
    let mut all = true;
    for provider in ["claude", "codex", "qwen", "grok"] {
        let mut cmd = Command::new(binary);
        cmd.args(["providers", "--host"]).arg(root).args([
            "--provider",
            provider,
            "check",
            "--mode",
            mode,
        ]);
        if let Some(identity) = identity {
            cmd.args(["--identity", identity]);
        }
        let row = match probe(&mut cmd, 5) {
            Ok(result) if result.status.success() => json!({"configured":true,
                "capability":if matches!(provider,"claude"|"codex") {"full hook dispatch"} else {"startup hook only"}}),
            Ok(result) => {
                all = false;
                json!({"configured":false,"detail":String::from_utf8_lossy(&result.stderr).chars().take(512).collect::<String>()})
            }
            Err(error) => {
                all = false;
                json!({"configured":false,"detail":error.to_string()})
            }
        };
        evidence.insert(provider.into(), row);
    }
    cell(
        "hook-config-contract",
        if all { "PASS" } else { "NOT_READY" },
        if all {
            "installed native provider configurations verified; qwen and grok provide startup hooks only"
        } else {
            "one or more native provider configurations unavailable or mismatched"
        },
        json!(evidence),
    )
}

pub(super) fn launcher_versions() -> Value {
    let mut missing = Vec::new();
    let mut versions = Map::new();
    for name in ["codex", "claude", "glm", "qwen", "grok"] {
        let mut cmd = Command::new(name);
        cmd.arg("--version");
        match probe(&mut cmd, 5) {
            Ok(output) if output.status.success() => {
                let text = if output.stdout.is_empty() {
                    output.stderr
                } else {
                    output.stdout
                };
                versions.insert(
                    name.into(),
                    json!(String::from_utf8_lossy(&text)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect::<String>()),
                );
            }
            Ok(output) => missing.push(format!("{name}:exit={}", output.status)),
            Err(error) => missing.push(format!("{name}:{error}")),
        }
    }
    dispatch(
        "check_launchers",
        &json!({"missing":missing,"versions":versions}),
    )
    .unwrap_or_else(|error| {
        cell(
            "launcher-programs",
            "FAIL-CLOSED",
            &error.to_string(),
            json!({}),
        )
    })
}

pub(super) fn graph(path: Option<&Path>) -> Value {
    let Some(path) = path else {
        return cell(
            "graph-semantic-runtime",
            "NOT_READY",
            "no graph evidence supplied",
            json!({}),
        );
    };
    let result = (|| -> anyhow::Result<Value> {
        let bytes = forge::bounded_file::read(path, 1024 * 1024)?;
        let mut payload: Value = forge::tool_schema::parse_document(std::str::from_utf8(&bytes)?)?;
        let object = payload
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("graph evidence must be an object"))?;
        object.insert(
            "source_sha256".into(),
            json!(format!("{:x}", Sha256::digest(&bytes))),
        );
        dispatch("check_graph_payload", &payload)
    })();
    result.unwrap_or_else(|error| {
        cell(
            "graph-semantic-runtime",
            "FAIL-CLOSED",
            &format!("graph evidence unreadable: {error:#}"),
            json!({}),
        )
    })
}

pub(super) fn retrievers(
    query: Option<&Path>,
    memory: Option<&Path>,
    cards: Option<&Path>,
    task: &str,
) -> Value {
    let (Some(query), Some(memory), Some(cards)) = (query, memory, cards) else {
        return cell(
            "retriever-runtime",
            "NOT_READY",
            "supply task-bound --recall-query, --memory-source and --cards",
            json!({}),
        );
    };
    let result = (|| -> anyhow::Result<Value> {
        let query = forge::recall::Query::load(query, task)?;
        let hits = forge::recall::memory_hits(memory, None, &query, 5)?;
        let cards = forge::recall::cards(cards, &query)?;
        Ok(
            json!({"memory_hits":hits.len(),"card_hits":cards.len(),"fingerprint":query.fingerprint()}),
        )
    })();
    match result {
        Ok(evidence)
            if evidence["memory_hits"].as_u64().unwrap_or(0) > 0
                && evidence["card_hits"].as_u64().unwrap_or(0) > 0 =>
        {
            cell(
                "retriever-runtime",
                "PASS",
                "native memory and card retrieval returned hits",
                evidence,
            )
        }
        Ok(evidence) => cell(
            "retriever-runtime",
            "FAIL-CLOSED",
            "a native retriever returned no hits",
            evidence,
        ),
        Err(error) => cell(
            "retriever-runtime",
            "NOT_READY",
            &format!("native retrieval unavailable: {error:#}"),
            json!({}),
        ),
    }
}

pub(super) fn hook_controls(binary: &Path, output: &Path) -> Value {
    let result = (|| -> anyhow::Result<Value> {
        let dir = output.join(forge::resource_id("hook-control")?);
        std::fs::create_dir(&dir)?;
        let guard = Fixture(dir);
        let mut observations = Map::new();
        for (name, target, expected) in [
            ("protected", ".current_work.md", true),
            ("safe", "README.md", false),
        ] {
            let payload = json!({"session_id":"workspace-runtime-test","tool_name":"Read","tool_input":{"file_path":target}});
            let mut cmd = Command::new(binary);
            cmd.args(["hook", "PreToolUse", "--host"])
                .arg(&guard.0)
                .args(["--protocol", "claude"])
                .env("FORGE_HOOK_STANDALONE", "1")
                .env("FORGE_MODE", "enforce")
                .env("LEDGER_ROOT", guard.0.join("ledger"))
                .current_dir(&guard.0);
            let out = forge::process_probe::run_with_input(
                &mut cmd,
                &serde_json::to_vec(&payload)?,
                Duration::from_secs(5),
                128 * 1024,
            )?;
            anyhow::ensure!(
                out.status.success(),
                "hook probe {name} exited {}",
                out.status
            );
            let response: Value = serde_json::from_slice(&out.stdout)?;
            let denied = response["hookSpecificOutput"]["permissionDecision"] == "deny";
            observations.insert(
                name.into(),
                json!({"denied":denied,"expected_deny":expected}),
            );
            anyhow::ensure!(
                denied == expected,
                "hook control {name} returned unexpected decision"
            );
        }
        Ok(json!(observations))
    })();
    match result {
        Ok(evidence) => cell(
            "hook-program-controls",
            "PASS",
            "native protected and safe read controls matched",
            evidence,
        ),
        Err(error) => cell(
            "hook-program-controls",
            "FAIL-CLOSED",
            &format!("native hook control failed: {error:#}"),
            json!({}),
        ),
    }
}
struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
