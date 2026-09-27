//! Path and Git-topology decisions for read-only project resolution.

use super::{NativeError, NativeResult};
use serde_json::{json, Value};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

fn argument<'a>(payload: &'a Value, name: &str) -> NativeResult<&'a str> {
    payload[name].as_str().ok_or_else(|| {
        NativeError::new(
            "INVALID_ARGUMENT",
            None,
            format!("missing {name} in native project context"),
        )
    })
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(super) fn decode_hex(text: &str) -> NativeResult<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(NativeError::new(
            "INVALID_ARGUMENT",
            None,
            "malformed native path bytes",
        ));
    }
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| match (hex_nibble(pair[0]), hex_nibble(pair[1])) {
            (Some(high), Some(low)) => Ok((high << 4) | low),
            _ => Err(NativeError::new(
                "INVALID_ARGUMENT",
                None,
                "malformed native path bytes",
            )),
        })
        .collect()
}

pub(super) fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 15) as usize] as char);
    }
    result
}

pub(super) fn path_from_hex(text: &str) -> NativeResult<PathBuf> {
    Ok(PathBuf::from(OsString::from_vec(decode_hex(text)?)))
}

pub(super) fn path_hex(path: &Path) -> String {
    encode_hex(path.as_os_str().as_bytes())
}

fn path_argument(payload: &Value, name: &str) -> NativeResult<PathBuf> {
    path_from_hex(argument(payload, name)?)
}

fn clean(path: &Path, field: &'static str) -> NativeResult<()> {
    if path
        .as_os_str()
        .as_bytes()
        .iter()
        .any(|byte| matches!(*byte, 0 | b'\r' | b'\n'))
    {
        return Err(NativeError::new(
            "INVALID_PATH",
            Some(field),
            format!("{field} resolves to a forbidden path character"),
        ));
    }
    Ok(())
}

fn canonical_directory(path: &Path, field: &'static str) -> NativeResult<PathBuf> {
    let resolved = fs::canonicalize(path).map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            NativeError::new(
                "PATH_NOT_FOUND",
                Some(field),
                format!("{field} does not exist"),
            )
        } else {
            NativeError::new(
                "INVALID_PATH",
                Some(field),
                format!("{field} cannot be resolved"),
            )
        }
    })?;
    if !resolved.is_dir() {
        return Err(NativeError::new(
            "PATH_NOT_DIRECTORY",
            Some(field),
            format!("{field} must be a directory"),
        ));
    }
    clean(&resolved, field)?;
    Ok(resolved)
}

pub(super) fn select_project(payload: &Value) -> NativeResult<Value> {
    let invocation = path_argument(payload, "invocation_hex")?;
    let argument = payload["argument_hex"].as_str();
    let environment = payload["environment_hex"].as_str();
    let (candidate, field, kind, source) = if let Some(raw) = argument {
        let path = path_from_hex(raw)?;
        let candidate = if path.is_absolute() {
            path
        } else {
            invocation.join(path)
        };
        (candidate, "project", "argument", "project")
    } else if let Some(raw) = environment {
        let bytes = decode_hex(raw)?;
        if bytes.is_empty() {
            return Err(NativeError::new(
                "INVALID_ARGUMENT",
                Some("CONDUCTOR_PROJECT_DIR"),
                "CONDUCTOR_PROJECT_DIR must not be empty",
            ));
        }
        let path = PathBuf::from(OsString::from_vec(bytes));
        if !path.is_absolute()
            || path
                .as_os_str()
                .as_bytes()
                .iter()
                .any(|byte| matches!(*byte, 0 | b'\r' | b'\n'))
        {
            return Err(NativeError::new(
                "INVALID_PATH",
                Some("CONDUCTOR_PROJECT_DIR"),
                "CONDUCTOR_PROJECT_DIR must be an absolute clean path",
            ));
        }
        (
            path,
            "CONDUCTOR_PROJECT_DIR",
            "environment",
            "CONDUCTOR_PROJECT_DIR",
        )
    } else {
        return Ok(json!({"selected_hex": path_hex(&invocation),
            "kind": "default", "source": "invocation directory"}));
    };
    let selected = canonical_directory(&candidate, field)?;
    Ok(json!({"selected_hex": path_hex(&selected), "kind": kind, "source": source}))
}

pub(super) fn git_probe(payload: &Value) -> NativeResult<Value> {
    let bytes = decode_hex(argument(payload, "raw_hex")?)?;
    if bytes == b"true\nfalse\n" {
        return Ok(json!({}));
    }
    if [
        b"false\nfalse\n".as_slice(),
        b"false\ntrue\n",
        b"true\ntrue\n",
    ]
    .contains(&bytes.as_slice())
    {
        return Err(NativeError::new(
            "UNSUPPORTED_REPOSITORY",
            Some("project"),
            "selected project is not a non-bare Git worktree",
        ));
    }
    Err(NativeError::new(
        "GIT_DISCOVERY_FAILED",
        Some("project"),
        "Git returned malformed worktree discovery output",
    ))
}

fn canonical_git_path(raw: &[u8]) -> NativeResult<PathBuf> {
    let path = PathBuf::from(OsString::from_vec(raw.to_vec()));
    if !path.is_absolute() || raw.iter().any(|byte| matches!(*byte, 0 | b'\r' | b'\n')) {
        return Err(NativeError::new(
            "GIT_DISCOVERY_FAILED",
            Some("project"),
            "Git returned an unreadable topology path",
        ));
    }
    let path = fs::canonicalize(path).map_err(|_| {
        NativeError::new(
            "GIT_DISCOVERY_FAILED",
            Some("project"),
            "Git returned an unreadable topology path",
        )
    })?;
    clean(&path, "project").map_err(|_| {
        NativeError::new(
            "GIT_DISCOVERY_FAILED",
            Some("project"),
            "Git returned an unreadable topology path",
        )
    })?;
    Ok(path)
}

pub(super) fn git_topology(payload: &Value) -> NativeResult<Value> {
    let bytes = decode_hex(argument(payload, "raw_hex")?)?;
    let trimmed = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let parts: Vec<&[u8]> = trimmed.split(|byte| *byte == b'\n').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(NativeError::new(
            "GIT_DISCOVERY_FAILED",
            Some("project"),
            "Git returned malformed topology output",
        ));
    }
    let paths: Vec<PathBuf> = parts
        .iter()
        .map(|part| canonical_git_path(part))
        .collect::<NativeResult<_>>()?;
    if paths.iter().any(|path| !path.is_dir()) {
        return Err(NativeError::new(
            "GIT_DISCOVERY_FAILED",
            Some("project"),
            "Git returned a non-directory topology path",
        ));
    }
    Ok(json!({"paths_hex": paths.iter().map(|path| path_hex(path)).collect::<Vec<_>>()}))
}

pub(super) fn git_membership(payload: &Value) -> NativeResult<Value> {
    let selected = path_argument(payload, "selected_hex")?;
    let top = path_argument(payload, "top_hex")?;
    if !selected.starts_with(&top) {
        return Err(NativeError::new(
            "PROJECT_MISMATCH",
            Some("project"),
            "selected directory is outside Git worktree top level",
        ));
    }
    Ok(json!({}))
}

pub(super) fn reference_inside(payload: &Value) -> NativeResult<Value> {
    let path = path_argument(payload, "path_hex")?;
    let root = path_argument(payload, "root_hex")?;
    let field = match argument(payload, "field")? {
        "config" => "config",
        "policy" => "policy",
        "registry" => "registry",
        _ => {
            return Err(NativeError::new(
                "INVALID_ARGUMENT",
                None,
                "unknown reference field",
            ))
        }
    };
    if !path.starts_with(&root) {
        return Err(NativeError::new(
            "PATH_OUTSIDE_PROJECT",
            Some(field),
            format!("{field} must remain inside the selected project"),
        ));
    }
    clean(&path, field)?;
    Ok(json!({}))
}

pub(super) fn config_ancestry(payload: &Value) -> NativeResult<Value> {
    let candidate = path_argument(payload, "candidate_hex")?;
    let root = path_argument(payload, "root_hex")?;
    let Ok(relative) = candidate.strip_prefix(&root) else {
        return Ok(json!({}));
    };
    let mut cursor = root;
    let components: Vec<&OsStr> = relative.iter().collect();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        cursor.push(component);
        let stat = match fs::symlink_metadata(&cursor) {
            Ok(stat) => stat,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(json!({})),
            Err(_) => {
                return Err(NativeError::new(
                    "CONFIG_IO",
                    Some("config"),
                    "configuration ancestry cannot be inspected",
                ))
            }
        };
        if stat.file_type().is_symlink() {
            let target = fs::canonicalize(&cursor).map_err(|_| {
                NativeError::new(
                    "CONFIG_IO",
                    Some("config"),
                    "configuration ancestry has a dangling symlink",
                )
            })?;
            if !target.is_dir() {
                return Err(NativeError::new(
                    "CONFIG_IO",
                    Some("config"),
                    "configuration ancestry is not a directory",
                ));
            }
        } else if !stat.is_dir() {
            return Err(NativeError::new(
                "CONFIG_IO",
                Some("config"),
                "configuration ancestry is not a directory",
            ));
        }
    }
    Ok(json!({}))
}

pub(super) fn safe_derived(payload: &Value) -> NativeResult<Value> {
    let common = path_argument(payload, "common_hex")?;
    let path = path_argument(payload, "path_hex")?;
    let field = match argument(payload, "field")? {
        "state_dir" => "state_dir",
        "cache_dir" => "cache_dir",
        "artifact_dir" => "artifact_dir",
        _ => {
            return Err(NativeError::new(
                "INVALID_ARGUMENT",
                None,
                "unknown derived directory field",
            ))
        }
    };
    let relative = path.strip_prefix(&common).map_err(|_| {
        NativeError::new(
            "UNSAFE_STATE_PATH",
            Some(field),
            format!("{field} has an unsafe existing ancestor"),
        )
    })?;
    let mut cursor = common;
    for component in relative.iter() {
        cursor.push(component);
        let stat = match fs::symlink_metadata(&cursor) {
            Ok(stat) => stat,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(json!({})),
            Err(_) => {
                return Err(NativeError::new(
                    "UNSAFE_STATE_PATH",
                    Some(field),
                    format!("{field} cannot be inspected"),
                ))
            }
        };
        if stat.file_type().is_symlink() || !stat.is_dir() {
            return Err(NativeError::new(
                "UNSAFE_STATE_PATH",
                Some(field),
                format!("{field} has an unsafe existing ancestor"),
            ));
        }
    }
    Ok(json!({}))
}

pub(super) fn note_authorized(payload: &Value) -> NativeResult<Value> {
    let path = path_argument(payload, "path_hex")?;
    let root = path_argument(payload, "root_hex")?;
    let allowed = payload["allowed_hex"].as_array().ok_or_else(|| {
        NativeError::new(
            "INVALID_ARGUMENT",
            None,
            "missing allowed roots in native project context",
        )
    })?;
    let inside = path.starts_with(&root)
        || allowed
            .iter()
            .filter_map(Value::as_str)
            .map(path_from_hex)
            .collect::<NativeResult<Vec<_>>>()?
            .iter()
            .any(|item| path.starts_with(item));
    if !inside {
        return Err(NativeError::new(
            "PATH_OUTSIDE_PROJECT",
            Some("paths.notes"),
            "external notes root lacks a caller grant",
        ));
    }
    Ok(json!({}))
}
