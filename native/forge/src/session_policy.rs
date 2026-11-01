//! Optional project-owned SessionStart text from the shared native manifest
//! reader. These strings provide context; they do not authorize actions.

use std::fmt;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

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
    let config = conductor_native::project_config::load(repo).map_err(SessionPolicyError)?;
    from_config(&config)
}

pub fn from_config(
    config: &conductor_native::project_config::ProjectConfig,
) -> Result<SessionPolicy, SessionPolicyError> {
    let path = &config.path;
    let Some(value) = config.conductor.get("session") else {
        return Ok(SessionPolicy::default());
    };
    let session = value
        .as_table()
        .ok_or_else(|| error(path, "session must be a table"))?;
    let keys: std::collections::BTreeSet<&str> = session.keys().map(String::as_str).collect();
    if keys != ["preamble", "standing_mandates"].into_iter().collect() {
        return Err(error(
            path,
            format!(
                "[{}.session] must contain exactly preamble and standing_mandates",
                config.namespace
            ),
        ));
    }
    Ok(SessionPolicy {
        preamble: strings(session.get("preamble"), path, "preamble")?,
        standing_mandates: strings(session.get("standing_mandates"), path, "standing_mandates")?,
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
