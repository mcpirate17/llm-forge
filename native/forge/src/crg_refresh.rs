//! Native port of `crg_graph_refresh.failure_output` (via
//! `crg_refresh_state.take_notices`): pure file I/O, no subprocess. Backs the
//! `crg_refresh_report_pre` hook, which surfaces a background graph-refresh
//! failure or warning once, on the next hook event, then clears the marker.
//!
//! Deliberately narrow: `crg_graph_refresh.py` also owns the queueing worker
//! (`_queue`, `drain`, `wait_for_fresh`, the `--worker` subprocess) that
//! *writes* `refresh.failed` -- none of that runs from a `PreToolUse` hook, so
//! none of it is ported here. This module only ever reads and clears the
//! marker file the worker (still Python, out of scope) may have left behind.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// `crg_refresh_state.store_for`: `CRG_DATA_DIR` when set, else
/// `<repo>/.code-review-graph`.
fn store_dir(repo_root: &Path) -> PathBuf {
    match std::env::var("CRG_DATA_DIR") {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw.trim()),
        _ => repo_root.join(".code-review-graph"),
    }
}

struct Notice {
    kind: String,
    text: String,
}

/// `crg_refresh_state.take_notices`: reads and deletes `refresh.failed`,
/// parsing each line as one JSON notice. A line that isn't valid JSON is kept
/// verbatim as a "failure" notice (mirrors Python's `except ValueError`
/// fallback). Absence of the file is not an error: `[]`.
fn take_notices(store: &Path) -> Vec<Notice> {
    let failed_path = store.join("refresh.failed");
    let Ok(text) = std::fs::read_to_string(&failed_path) else {
        return Vec::new();
    };
    let _ = std::fs::remove_file(&failed_path);
    let mut notices = Vec::new();
    for raw in text.lines() {
        if raw.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(raw) {
            Ok(item) if item.is_object() => {
                let kind = item
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("failure")
                    .to_string();
                let paths: Vec<String> = item
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let body = item
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("error").and_then(Value::as_str))
                    .unwrap_or("");
                let suffix = if kind == "failure" {
                    format!(" while refreshing {}", format_paths(&paths))
                } else {
                    String::new()
                };
                notices.push(Notice {
                    kind,
                    text: format!("{body}{suffix}"),
                });
            }
            _ => notices.push(Notice {
                kind: "failure".to_string(),
                text: raw.to_string(),
            }),
        }
    }
    notices
}

/// Python's `f"{item['paths']}"` for a list of strings: `['a', 'b']` (repr
/// quoting, comma-space separated).
fn format_paths(paths: &[String]) -> String {
    let quoted: Vec<String> = paths.iter().map(|p| format!("'{p}'")).collect();
    format!("[{}]", quoted.join(", "))
}

/// `failure_output(event)`: `Value::Null` when there is nothing to report,
/// else the advisory `hookSpecificOutput`/`systemMessage` pair.
pub fn failure_output(event: &str, repo_root: &Path) -> Value {
    let notices = take_notices(&store_dir(repo_root));
    if notices.is_empty() {
        return Value::Null;
    }
    let failures: Vec<&str> = notices
        .iter()
        .filter(|n| n.kind == "failure")
        .map(|n| n.text.as_str())
        .collect();
    let warnings: Vec<&str> = notices
        .iter()
        .filter(|n| n.kind != "failure")
        .map(|n| n.text.as_str())
        .collect();
    let mut parts = Vec::new();
    if !failures.is_empty() {
        parts.push(format!(
            "WARNING: background graph refresh FAILED: {}. Graph reads are STALE until \
             `code-review-graph update` succeeds.",
            failures.join("; ")
        ));
    }
    if !warnings.is_empty() {
        parts.push(format!(
            "WARNING: background graph refresh: {}",
            warnings.join("; ")
        ));
    }
    let message = parts.join(" ");
    json!({
        "hookSpecificOutput": {"hookEventName": event, "additionalContext": message},
        "systemMessage": message,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    /// `store_dir` reads the process-global `CRG_DATA_DIR` env var, so every
    /// test in this module must be serialized against the one test that
    /// mutates it -- otherwise a concurrently running test can observe the
    /// wrong store directory (see `handlers.rs` for the same env-var-test
    /// convention; this module additionally needs the lock because more than
    /// one of its tests reads that same var indirectly through `store_dir`).
    ///
    /// `pub(crate)` (module and lock both) so that any other `#[path]`-included
    /// test binary that pulls this file in (e.g.
    /// `tests/bash_pretooluse_hooks_parity.rs`) and *also* mutates
    /// `CRG_DATA_DIR` from its own top-level test can serialize against this
    /// same lock instead of racing it with an unrelated `Mutex` of its own --
    /// two different `Mutex` instances guarding the same env var provide no
    /// mutual exclusion at all.
    pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Local RAII temp directory (this crate avoids the `tempfile` crate; see
    /// `bash_impact.rs`/`interpreter.rs` for the same convention).
    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "forge-crg-refresh-test-{}-{label}-{n}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            ScratchDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_marker(repo: &Path, lines: &[&str]) {
        let dir = repo.join(".code-review-graph");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("refresh.failed"), lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn no_marker_is_null() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = ScratchDir::new("no-marker");
        assert_eq!(failure_output("PreToolUse", tmp.path()), Value::Null);
    }

    #[test]
    fn failure_notice_reports_and_clears() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = ScratchDir::new("failure");
        write_marker(
            tmp.path(),
            &[r#"{"kind":"failure","paths":["a.py"],"text":"RuntimeError: boom"}"#],
        );
        let out = failure_output("PreToolUse", tmp.path());
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains("FAILED"));
        assert!(context.contains("boom"));
        assert!(context.contains("'a.py'"));
        // Consumed: a second read sees nothing.
        assert_eq!(failure_output("PreToolUse", tmp.path()), Value::Null);
    }

    #[test]
    fn warning_notice_reports_without_failed_language() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = ScratchDir::new("warning");
        write_marker(
            tmp.path(),
            &[r#"{"kind":"warning","paths":[],"text":"embeddings skipped"}"#],
        );
        let out = failure_output("PreToolUse", tmp.path());
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains("embeddings skipped"));
        assert!(!context.contains("FAILED"));
    }

    #[test]
    fn unparseable_line_falls_back_to_a_raw_failure_notice() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = ScratchDir::new("unparseable");
        write_marker(tmp.path(), &["not json at all"]);
        let out = failure_output("PreToolUse", tmp.path());
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains("not json at all"));
    }

    #[test]
    fn crg_data_dir_env_override_is_honored() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = ScratchDir::new("root");
        let alt = ScratchDir::new("alt");
        std::env::set_var("CRG_DATA_DIR", alt.path());
        fs::write(
            alt.path().join("refresh.failed"),
            r#"{"kind":"failure","paths":[],"text":"x"}"#.to_string() + "\n",
        )
        .unwrap();
        let out = failure_output("PreToolUse", tmp.path());
        std::env::remove_var("CRG_DATA_DIR");
        assert!(out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains('x'));
    }
}
