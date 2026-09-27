//! Stable repository identity and bounded, read-only layout decisions.

use super::paths::{decode_hex, path_from_hex, path_hex};
use super::{NativeError, NativeResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn argument<'a>(payload: &'a Value, name: &str) -> NativeResult<&'a str> {
    payload[name].as_str().ok_or_else(|| {
        NativeError::new(
            "INVALID_ARGUMENT",
            None,
            format!("missing {name} in native project context"),
        )
    })
}

fn key(prefix: &str, payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    format!("{prefix}{:x}", hasher.finalize())
}

pub(super) fn keys(payload: &Value) -> NativeResult<Value> {
    let common = decode_hex(argument(payload, "common_hex")?)?;
    let git = decode_hex(argument(payload, "git_hex")?)?;
    let root = decode_hex(argument(payload, "root_hex")?)?;
    let repository_key = key(
        "repo-v1-",
        &[b"conductor.repository.v1\0".as_slice(), &common].concat(),
    );
    let worktree_key = key(
        "wt-v1-",
        &[b"conductor.worktree.v1\0".as_slice(), &git, b"\0", &root].concat(),
    );
    Ok(json!({"repository_key": repository_key, "worktree_key": worktree_key}))
}

pub(super) fn derived_layout(payload: &Value) -> NativeResult<Value> {
    let common = path_from_hex(argument(payload, "common_hex")?)?;
    let repository_key = argument(payload, "repository_key")?;
    let worktree_key = argument(payload, "worktree_key")?;
    if !repository_key.starts_with("repo-v1-")
        || !worktree_key.starts_with("wt-v1-")
        || repository_key.contains('/')
        || worktree_key.contains('/')
    {
        return Err(NativeError::new(
            "INVALID_ARGUMENT",
            None,
            "invalid repository or worktree key for native project context",
        ));
    }
    let state = common
        .join("conductor")
        .join("projects")
        .join(repository_key);
    let worktree = state.join("worktrees").join(worktree_key);
    Ok(json!({"state_hex": path_hex(&state),
        "cache_hex": path_hex(&worktree.join("cache")),
        "artifact_hex": path_hex(&worktree.join("artifacts"))}))
}

pub(super) fn config_read(payload: &Value) -> NativeResult<Value> {
    let size = payload["size"]
        .as_u64()
        .ok_or_else(|| NativeError::new("INVALID_ARGUMENT", None, "missing configuration size"))?;
    if size > 65_536 {
        return Err(NativeError::new(
            "CONFIG_TOO_LARGE",
            Some("config"),
            "configuration exceeds 65536 bytes",
        ));
    }
    let signatures = payload["signatures"].as_array().ok_or_else(|| {
        NativeError::new(
            "INVALID_ARGUMENT",
            None,
            "missing configuration read signatures",
        )
    })?;
    if signatures.len() != 3 || signatures.iter().any(|item| item != &signatures[0]) {
        return Err(NativeError::new(
            "INPUT_CHANGED",
            Some("config"),
            "configuration changed while it was read",
        ));
    }
    Ok(json!({}))
}
