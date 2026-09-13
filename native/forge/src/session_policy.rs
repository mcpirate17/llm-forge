//! The optional project-owned SessionStart policy, ported from
//! `conductor.session_preamble`'s Python twin: a host opts in by putting
//! complete `[tool.conductor.session]` text in its root `pyproject.toml`.
//! This module selects and validates those strings without interpreting
//! them as authority -- every rule and message mirrors the Python.

use std::fmt;
use std::path::{Path, PathBuf};

const CONFIG_LIMIT: u64 = 64 * 1024;

#[derive(Debug, Default, Clone)]
pub struct SessionPolicy {
    pub preamble: Vec<String>,
    pub standing_mandates: Vec<String>,
}

#[derive(Debug)]
pub struct SessionPolicyError(String);

impl fmt::Display for SessionPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SessionPolicyError {}

fn error(path: &Path, message: impl fmt::Display) -> SessionPolicyError {
    SessionPolicyError(format!("{}: {}", path.display(), message))
}

/// `pyproject.toml` as a parsed table, `None` when the file is absent --
/// a package install has no project policy. Every other failure is loud,
/// with the path in the message, like the Python.
fn read_config(path: &Path) -> Result<Option<toml::Value>, SessionPolicyError> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(error(path, format!("cannot inspect configuration: {err}"))),
    };
    if !metadata.is_file() {
        return Err(error(path, "configuration must be a regular file"));
    }
    if metadata.len() > CONFIG_LIMIT {
        return Err(error(path, "configuration exceeds 64 KiB"));
    }
    let raw = std::fs::read(path)
        .map_err(|err| error(path, format!("cannot read configuration: {err}")))?;
    if raw.len() as u64 > CONFIG_LIMIT {
        return Err(error(path, "configuration exceeds 64 KiB"));
    }
    let text = String::from_utf8(raw)
        .map_err(|_| error(path, "invalid TOML configuration: invalid utf-8"))?;
    text.parse::<toml::Value>()
        .map(Some)
        .map_err(|err| error(path, format!("invalid TOML configuration: {err}")))
}

/// `None` when the key is absent, an error when it is present but not a
/// table -- `[tool]` in a pyproject always is, so in practice only a
/// hand-mangled file hits the error arm.
fn table<'a>(
    parent: Option<&'a toml::Value>,
    key: &str,
    path: &Path,
    label: &str,
) -> Result<Option<&'a toml::Value>, SessionPolicyError> {
    match parent.and_then(|value| value.get(key)) {
        None => Ok(None),
        Some(value) if value.is_table() => Ok(Some(value)),
        Some(_) => Err(error(path, format!("{label} must be a table"))),
    }
}

fn strings(
    value: Option<&toml::Value>,
    path: &Path,
    field: &str,
) -> Result<Vec<String>, SessionPolicyError> {
    let Some(array) = value.and_then(toml::Value::as_array) else {
        return Err(error(
            path,
            format!("{field} must be a list of nonempty strings"),
        ));
    };
    let mut out = Vec::with_capacity(array.len());
    for item in array {
        let text = item.as_str().filter(|text| !text.trim().is_empty());
        match text {
            Some(text) => out.push(text.to_owned()),
            None => {
                return Err(error(
                    path,
                    format!("{field} must be a list of nonempty strings"),
                ))
            }
        }
    }
    Ok(out)
}

/// The complete project session policy, or the generic empty policy when
/// the file, table or both are absent.
pub fn load_session_policy(repo: &Path) -> Result<SessionPolicy, SessionPolicyError> {
    if !repo.is_dir() {
        return Err(SessionPolicyError(format!(
            "{}: repository must be an existing directory",
            repo.display()
        )));
    }
    let path: PathBuf = repo.join("pyproject.toml");
    let Some(payload) = read_config(&path)? else {
        return Ok(SessionPolicy::default());
    };
    let tool = table(Some(&payload), "tool", &path, "[tool]")?;
    let conductor = table(tool, "conductor", &path, "[tool.conductor]")?;
    let session = table(conductor, "session", &path, "[tool.conductor.session]")?;
    let Some(session) = session else {
        return Ok(SessionPolicy::default());
    };
    let keys: std::collections::BTreeSet<&str> = session
        .as_table()
        .expect("table() only returns table values")
        .keys()
        .map(String::as_str)
        .collect();
    if keys != ["preamble", "standing_mandates"].into_iter().collect() {
        return Err(error(
            &path,
            "[tool.conductor.session] must contain exactly preamble and standing_mandates",
        ));
    }
    Ok(SessionPolicy {
        preamble: strings(session.get("preamble"), &path, "preamble")?,
        standing_mandates: strings(session.get("standing_mandates"), &path, "standing_mandates")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct ScratchDir(PathBuf);
    impl ScratchDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "forge-session-policy-test-{label}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            ScratchDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_missing_pyproject_is_an_empty_policy() {
        let scratch = ScratchDir::new("missing");
        let policy = load_session_policy(scratch.path()).unwrap();
        assert!(policy.preamble.is_empty());
        assert!(policy.standing_mandates.is_empty());
    }

    #[test]
    fn a_full_table_loads_both_lists() {
        let scratch = ScratchDir::new("full");
        std::fs::write(
            scratch.path().join("pyproject.toml"),
            "[tool.conductor.session]\npreamble = [\"line one\", \"line two\"]\nstanding_mandates = [\"KB-1: always\"]\n",
        )
        .unwrap();
        let policy = load_session_policy(scratch.path()).unwrap();
        assert_eq!(policy.preamble, vec!["line one", "line two"]);
        assert_eq!(policy.standing_mandates, vec!["KB-1: always"]);
    }

    #[test]
    fn a_non_string_entry_is_an_error_naming_the_path() {
        let scratch = ScratchDir::new("badtype");
        std::fs::write(
            scratch.path().join("pyproject.toml"),
            "[tool.conductor.session]\npreamble = [1]\nstanding_mandates = []\n",
        )
        .unwrap();
        let err = load_session_policy(scratch.path()).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("pyproject.toml"), "{message}");
        assert!(
            message.contains("preamble must be a list of nonempty strings"),
            "{message}"
        );
    }

    #[test]
    fn an_oversized_configuration_is_rejected() {
        let scratch = ScratchDir::new("toobig");
        let pad = "x".repeat(65 * 1024);
        std::fs::write(
            scratch.path().join("pyproject.toml"),
            format!("[tool]\nother = \"{pad}\"\n"),
        )
        .unwrap();
        let err = load_session_policy(scratch.path()).unwrap_err();
        assert!(
            err.to_string().contains("configuration exceeds 64 KiB"),
            "{}",
            err
        );
    }
}
