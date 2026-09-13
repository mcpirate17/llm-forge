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

/// The interpreter the Python dispatcher should run under: the project's own
/// `.venv/bin/python` when one exists (so it imports the same `conductor`/`tooling`
/// forge does), else `python3` from `PATH`. Mirrors
/// `tooling.hooks.dispatch.paths.own_interpreter`'s venv-over-PATH preference.
pub fn resolve_python(root: &Path) -> PathBuf {
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
        assert_eq!(resolve_python(&tmp), python);
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
