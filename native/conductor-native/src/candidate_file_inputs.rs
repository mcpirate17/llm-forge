//! File-input contract for external candidate checks.
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use serde_json::Value;
use std::path::{Component, Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileInput {
    Paths,
    CppcheckList,
}

pub(crate) fn parse(policy: &Value) -> Result<FileInput, String> {
    let protocol = match policy.get("file_input") {
        None => FileInput::Paths,
        Some(Value::String(s)) if s == "paths" => FileInput::Paths,
        Some(Value::String(s)) if s == "cppcheck-list" => FileInput::CppcheckList,
        _ => return Err("file_input must be 'paths' or 'cppcheck-list'".into()),
    };
    if protocol == FileInput::CppcheckList {
        if policy.get("kind").and_then(Value::as_str) != Some("command") {
            return Err("cppcheck-list requires a command check".into());
        }
        let command = policy
            .get("command")
            .and_then(Value::as_array)
            .ok_or("cppcheck-list command must be an array")?;
        let mut count = 0;
        for arg in command {
            let arg = arg
                .as_str()
                .ok_or("cppcheck-list command arguments must be strings")?;
            if arg == "--file-list={changed_files_file}" {
                count += 1;
            } else if arg.contains("{changed_files_file}")
                || arg.starts_with("--file-list=")
                || arg == "--file-list"
            {
                return Err(
                    "cppcheck-list requires exactly one canonical file-list argument".into(),
                );
            }
            if arg.contains("{files}") {
                return Err("cppcheck-list forbids {files} arguments".into());
            }
        }
        if count != 1 {
            return Err("cppcheck-list requires exactly one canonical file-list argument".into());
        }
    }
    Ok(protocol)
}

pub(crate) fn path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 4096
        || path.starts_with('/')
        || path.contains(['\\', '\0', '\r', '\n'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("cppcheck-list input path must be bounded canonical relative UTF-8 without backslashes or line breaks".into());
    }
    Ok(())
}

fn check_file(root: &Dir, raw: &str) -> Result<(), String> {
    let path = Path::new(raw);
    let parent = path.parent().ok_or("input path has no parent")?;
    let name = path.file_name().ok_or("input path has no filename")?;
    let mut dir = root
        .try_clone()
        .map_err(|e| format!("clone input root: {e}"))?;
    for part in parent.components() {
        let Component::Normal(name) = part else {
            return Err("invalid input parent".into());
        };
        let mut options = OpenOptions::new();
        options.read(true).custom_flags(
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        );
        dir = Dir::from_std_file(
            dir.open_with(name, &options)
                .map_err(|e| format!("open input parent {raw}: {e}"))?
                .into_std(),
        );
    }
    let metadata = dir
        .symlink_metadata(name)
        .map_err(|e| format!("inspect input {raw}: {e}"))?;
    if !metadata.is_file() {
        return Err(format!("cppcheck-list input is not a regular file: {raw}"));
    }
    Ok(())
}

/// Preflight selected file-list entries relative to an operator-pinned snapshot.
/// Snapshot and parent owners must leave the selected inodes immutable during execution.
pub fn preflight_file_inputs(policy: &Value, files: &[&str], snapshot: &Dir) -> Result<(), String> {
    if parse(policy)? == FileInput::Paths {
        return Ok(());
    }
    if files.len() > 100_000 {
        return Err("file-input count exceeds 100000".into());
    }
    let mut total = 0usize;
    for &file in files {
        path(file)?;
        total = total
            .checked_add(file.len() + 1)
            .ok_or("file-input size overflow")?;
        if total > 8 * 1024 * 1024 {
            return Err("file-input paths exceed 8 MiB".into());
        }
    }
    for &file in files {
        check_file(snapshot, file)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "candidate_file_inputs_tests.rs"]
mod tests;
