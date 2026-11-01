//! Runtime state belongs in explicit storage or the user's XDG state directory.
use anyhow::{ensure, Context, Result};
use std::ffi::OsString;
use std::path::PathBuf;

fn resolve_state(xdg: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    let root = match xdg.filter(|s| !s.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(
            home.filter(|s| !s.is_empty())
                .context("set XDG_STATE_HOME or HOME to resolve persistent Forge state")?,
        )
        .join(".local/state"),
    };
    ensure!(
        root.is_absolute(),
        "XDG_STATE_HOME/HOME must be an absolute path: {}",
        root.display()
    );
    Ok(root.join("llm-forge"))
}

pub fn state_root() -> Result<PathBuf> {
    resolve_state(
        std::env::var_os("XDG_STATE_HOME"),
        std::env::home_dir().map(PathBuf::into_os_string),
    )
}

pub fn ledger_root(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit.or_else(|| std::env::var_os("LEDGER_ROOT").map(PathBuf::from)) {
        ensure!(
            !path.as_os_str().is_empty(),
            "ledger root must not be empty"
        );
        return Ok(path);
    }
    Ok(state_root()?.join("ledger"))
}

pub fn telemetry_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CONTEXT_TELEMETRY_PATH") {
        ensure!(!path.is_empty(), "CONTEXT_TELEMETRY_PATH must not be empty");
        return Ok(PathBuf::from(path));
    }
    Ok(ledger_root(None)?.join("telemetry/context_telemetry/events.jsonl"))
}

pub fn fragments_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CONTEXT_FRAGMENT_DIR") {
        ensure!(!path.is_empty(), "CONTEXT_FRAGMENT_DIR must not be empty");
        return Ok(PathBuf::from(path));
    }
    Ok(ledger_root(None)?.join("context_fragments"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_state_precedence_and_invalid_configuration_are_explicit() {
        assert_eq!(
            resolve_state(Some("/tmp/state".into()), Some("/tmp/home".into())).unwrap(),
            PathBuf::from("/tmp/state/llm-forge")
        );
        assert_eq!(
            resolve_state(None, Some("/tmp/home".into())).unwrap(),
            PathBuf::from("/tmp/home/.local/state/llm-forge")
        );
        assert!(resolve_state(None, None).is_err());
        assert!(resolve_state(Some("relative".into()), Some("/tmp/home".into())).is_err());
        assert!(resolve_state(None, Some("relative".into())).is_err());
    }
}
