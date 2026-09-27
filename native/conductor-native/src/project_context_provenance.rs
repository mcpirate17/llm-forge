//! Ordered project-context provenance and project identity decisions.

use super::{NativeError, NativeResult};
use serde_json::{json, Value};

fn string<'a>(payload: &'a Value, name: &str) -> NativeResult<&'a str> {
    payload[name].as_str().ok_or_else(|| {
        NativeError::new(
            "INVALID_ARGUMENT",
            None,
            format!("missing {name} in native project context"),
        )
    })
}

fn optional<'a>(payload: &'a Value, name: &str) -> NativeResult<Option<&'a str>> {
    if payload[name].is_null() {
        Ok(None)
    } else {
        string(payload, name).map(Some)
    }
}

fn flag(payload: &Value, name: &str) -> NativeResult<bool> {
    payload[name].as_bool().ok_or_else(|| {
        NativeError::new(
            "INVALID_ARGUMENT",
            None,
            format!("missing {name} in native project context"),
        )
    })
}

fn literal(text: impl Into<String>) -> Value {
    json!({"text": text.into()})
}
fn path(hex: &str) -> Value {
    json!({"path_hex": hex})
}
fn sourced_path(hex: &str, suffix: &str) -> Value {
    json!({"path_hex": hex, "suffix": suffix})
}

fn entry(field: &str, kind: &str, source: Value, value: Value, digest: Option<&str>) -> Value {
    json!({"field":field, "kind":kind, "source":source,
        "value":value, "config_sha256":digest})
}

fn topology(payload: &Value) -> NativeResult<Vec<Value>> {
    let root = string(payload, "root_hex")?;
    let git = optional(payload, "git_hex")?;
    let common = optional(payload, "common_hex")?;
    let is_git = git.is_some();
    Ok(vec![
        entry(
            "worktree_root",
            if is_git { "git" } else { "argument" },
            literal(if is_git {
                "git:show-toplevel"
            } else {
                "project"
            }),
            path(root),
            None,
        ),
        entry(
            "git_dir",
            if is_git { "git" } else { "default" },
            literal(if is_git {
                "git:absolute-git-dir"
            } else {
                "read_only"
            }),
            git.map(path).unwrap_or(Value::Null),
            None,
        ),
        entry(
            "git_common_dir",
            if is_git { "git" } else { "default" },
            literal(if is_git {
                "git:git-common-dir"
            } else {
                "read_only"
            }),
            common.map(path).unwrap_or(Value::Null),
            None,
        ),
    ])
}

fn identity(payload: &Value) -> NativeResult<(Option<String>, Vec<Value>)> {
    let repository_key = optional(payload, "repository_key")?;
    let worktree_key = optional(payload, "worktree_key")?;
    let configured_id = optional(payload, "config_project_id")?;
    let config_path = optional(payload, "config_path_hex")?;
    let digest = optional(payload, "config_sha256")?;
    let derived = repository_key.is_some();
    let project_id = configured_id.or(repository_key).map(str::to_owned);
    let kind = if configured_id.is_some() {
        "config"
    } else if derived {
        "derived"
    } else {
        "default"
    };
    let source = if configured_id.is_some() {
        sourced_path(
            config_path.ok_or_else(|| {
                NativeError::new(
                    "INVALID_ARGUMENT",
                    None,
                    "configured project id requires a config path",
                )
            })?,
            "#project.id",
        )
    } else {
        literal(if derived {
            "repository_key"
        } else {
            "read_only"
        })
    };
    let rows = vec![
        entry(
            "repository_key",
            if derived { "derived" } else { "default" },
            literal(if derived {
                "repository_key"
            } else {
                "read_only"
            }),
            repository_key.map(literal).unwrap_or(Value::Null),
            None,
        ),
        entry(
            "worktree_key",
            if derived { "derived" } else { "default" },
            literal(if derived { "worktree_key" } else { "read_only" }),
            worktree_key.map(literal).unwrap_or(Value::Null),
            None,
        ),
        entry(
            "project_id",
            kind,
            source,
            project_id.as_deref().map(literal).unwrap_or(Value::Null),
            if configured_id.is_some() {
                digest
            } else {
                None
            },
        ),
    ];
    Ok((project_id, rows))
}

fn config(payload: &Value) -> NativeResult<Value> {
    let config_hex = optional(payload, "config_path_hex")?;
    let candidate = string(payload, "config_candidate_hex")?;
    let explicit = flag(payload, "config_explicit")?;
    let digest = optional(payload, "config_sha256")?;
    Ok(entry(
        "config_path",
        if explicit { "argument" } else { "default" },
        config_hex.map(path).unwrap_or_else(|| path(candidate)),
        config_hex.map(path).unwrap_or(Value::Null),
        if config_hex.is_some() { digest } else { None },
    ))
}

fn reference(payload: &Value, field: &str) -> NativeResult<Value> {
    let path_hex = string(payload, &format!("{field}_path_hex"))?;
    let root = string(payload, "root_hex")?;
    let config_path = optional(payload, "config_path_hex")?;
    let digest = optional(payload, "config_sha256")?;
    let argument = flag(payload, &format!("{field}_argument"))?;
    let configured = flag(payload, &format!("{field}_configured"))?;
    let (kind, source, sha) = if argument {
        ("argument", literal(field), None)
    } else if configured {
        (
            "config",
            sourced_path(
                config_path.ok_or_else(|| {
                    NativeError::new(
                        "INVALID_ARGUMENT",
                        None,
                        "configured reference requires a config path",
                    )
                })?,
                &format!("#paths.{field}"),
            ),
            digest,
        )
    } else {
        let default =
            super::paths::path_from_hex(root)?
                .join(".conductor")
                .join(if field == "policy" {
                    "policy.toml"
                } else {
                    "mutation/registry.json"
                });
        ("default", path(&super::paths::path_hex(&default)), None)
    };
    Ok(entry(
        &format!("{field}_path"),
        kind,
        source,
        path(path_hex),
        sha,
    ))
}

fn state(payload: &Value) -> NativeResult<Vec<Value>> {
    let state = optional(payload, "state_hex")?;
    let cache = optional(payload, "cache_hex")?;
    let artifact = optional(payload, "artifact_hex")?;
    Ok(vec![
        entry(
            "state_dir",
            if state.is_some() {
                "derived"
            } else {
                "default"
            },
            literal(if state.is_some() {
                "git_common_dir"
            } else {
                "read_only"
            }),
            state.map(path).unwrap_or(Value::Null),
            None,
        ),
        entry(
            "cache_dir",
            if cache.is_some() {
                "derived"
            } else {
                "default"
            },
            literal(if cache.is_some() {
                "state_dir/worktree_key"
            } else {
                "read_only"
            }),
            cache.map(path).unwrap_or(Value::Null),
            None,
        ),
        entry(
            "artifact_dir",
            if artifact.is_some() {
                "derived"
            } else {
                "default"
            },
            literal(if artifact.is_some() {
                "state_dir/worktree_key"
            } else {
                "read_only"
            }),
            artifact.map(path).unwrap_or(Value::Null),
            None,
        ),
    ])
}

fn notes(payload: &Value) -> NativeResult<Value> {
    let configured = flag(payload, "notes_configured")?;
    let config_path = optional(payload, "config_path_hex")?;
    let digest = optional(payload, "config_sha256")?;
    let paths = payload["notes_hex"].as_array().ok_or_else(|| {
        NativeError::new(
            "INVALID_ARGUMENT",
            None,
            "missing notes roots in native project context",
        )
    })?;
    if paths.iter().any(|item| !item.is_string()) {
        return Err(NativeError::new(
            "INVALID_ARGUMENT",
            None,
            "malformed notes roots in native project context",
        ));
    }
    let source = if configured {
        sourced_path(
            config_path.ok_or_else(|| {
                NativeError::new(
                    "INVALID_ARGUMENT",
                    None,
                    "configured notes require a config path",
                )
            })?,
            "#paths.notes",
        )
    } else {
        literal("empty notes default")
    };
    Ok(entry(
        "notes_roots",
        if configured { "config" } else { "default" },
        source,
        json!({"paths_hex":paths}),
        if configured { digest } else { None },
    ))
}

pub(super) fn assemble(payload: &Value) -> NativeResult<Value> {
    let mode = string(payload, "mode")?;
    if !matches!(mode, "git" | "read_only") {
        return Err(NativeError::new(
            "INVALID_ARGUMENT",
            Some("mode"),
            "mode must be 'git' or 'read_only'",
        ));
    }
    let selection_kind = string(payload, "selection_kind")?;
    let selection_source = string(payload, "selection_source")?;
    let root = string(payload, "root_hex")?;
    let mut rows = vec![
        entry("mode", "argument", literal("mode"), literal(mode), None),
        entry(
            "repo_root",
            selection_kind,
            literal(selection_source),
            path(root),
            None,
        ),
    ];
    rows.extend(topology(payload)?);
    let (project_id, identity_rows) = identity(payload)?;
    rows.extend(identity_rows);
    rows.push(config(payload)?);
    rows.push(reference(payload, "policy")?);
    rows.push(reference(payload, "registry")?);
    rows.extend(state(payload)?);
    rows.push(notes(payload)?);
    Ok(json!({"project_id":project_id, "provenance":rows}))
}
