//! Resolving the project root and the Python interpreter that carries `conductor`
//! and `tooling`, mirroring `tooling.hooks.dispatch.paths` and `__main__._root`
//! without re-exec: forge is the process starting fresh, not one already running
//! under some other interpreter that needs to hand off.

use std::env;
use std::path::{Path, PathBuf};

/// The project root: `CLAUDE_PROJECT_DIR` when Claude Code sets it (the same
/// variable `tooling.hooks.dispatch.__main__._root` checks first), else the
/// directory forge was invoked from. Never guesses further -- an unset
/// `CLAUDE_PROJECT_DIR` outside of Claude Code just means "run from here", the same
/// fallback `os.getcwd()`-relative behavior the Python launcher's rendered script
/// gets from its own `Path(__file__).resolve().parents[2]`, except forge has no
/// fixed install location to derive one from.
pub fn project_root() -> PathBuf {
    match env::var("CLAUDE_PROJECT_DIR") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

/// The interpreter the Python dispatcher should run under: the one a mutation
/// snapshot exported (`CONDUCTOR_SNAPSHOT_PYTHON` -- snapshots are git trees and
/// `.venv` is gitignored, so the host hands its interpreter over), else the
/// project's `.venv/bin/python` when one exists (so it imports the same
/// `conductor`/`tooling` forge does), else `python3` from `PATH`. Mirrors
/// `tooling.hooks.dispatch.paths.own_interpreter`'s venv-over-PATH preference,
/// with the snapshot export ranked above both because inside a sandbox it is
/// the only interpreter that can import `conductor` at all.
pub fn resolve_python(root: &Path) -> PathBuf {
    let snapshot = env::var("CONDUCTOR_SNAPSHOT_PYTHON")
        .ok()
        .filter(|value| !value.trim().is_empty());
    resolve_python_with(root, snapshot.as_deref())
}

/// `resolve_python` with the snapshot export supplied instead of read from the
/// environment, so the preference order is testable without racing every other
/// test that might touch the process environment.
fn resolve_python_with(root: &Path, snapshot_python: Option<&str>) -> PathBuf {
    if let Some(value) = snapshot_python {
        return PathBuf::from(value);
    }
    let venv_python = root.join(".venv").join("bin").join("python");
    if venv_python.is_file() {
        return venv_python;
    }
    PathBuf::from("python3")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_project_venv_when_present() {
        let tmp = std::env::temp_dir().join(format!("forge-interp-test-{}", std::process::id()));
        let venv_bin = tmp.join(".venv").join("bin");
        std::fs::create_dir_all(&venv_bin).unwrap();
        let python = venv_bin.join("python");
        std::fs::write(&python, b"#!/bin/sh\n").unwrap();
        assert_eq!(resolve_python_with(&tmp, None), python);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn a_snapshot_export_beats_the_venv_and_the_path_fallback() {
        // A mutation snapshot is a git tree, so it has no .venv -- the export is
        // the only interpreter in it that can import conductor. It must win even
        // against a venv, because the venv belongs to a different tree (the host).
        let tmp =
            std::env::temp_dir().join(format!("forge-interp-test-export-{}", std::process::id()));
        let venv_bin = tmp.join(".venv").join("bin");
        std::fs::create_dir_all(&venv_bin).unwrap();
        std::fs::write(venv_bin.join("python"), b"#!/bin/sh\n").unwrap();
        assert_eq!(
            resolve_python_with(&tmp, Some("/host/.venv/bin/python")),
            PathBuf::from("/host/.venv/bin/python")
        );
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn falls_back_to_python3_without_a_venv() {
        let tmp =
            std::env::temp_dir().join(format!("forge-interp-test-novenv-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(resolve_python(&tmp), PathBuf::from("python3"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
