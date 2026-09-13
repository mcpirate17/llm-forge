//! Native port of `crg_graph_refresh`'s PostToolUse surface:
//! `failure_output` (via `crg_refresh_state.take_notices`) for
//! `crg_refresh_report_post`, and `_queue([FULL_UPDATE])` /
//! `full_update_output` for `post_bash_graph` -- queue a whole-tree refresh
//! by appending to the graph store's `refresh.pending` marker and spawning
//! one detached worker when none is running, then return at once.
//!
//! The worker itself stays Python (`crg_graph_refresh.py --worker`): it owns
//! the debounce/coalesce loop, the bounded refresh child and the embedding
//! bridge, none of which belongs on a sub-2ms hook path. What is ported is
//! the queueing half exactly -- the `refresh.pending` append and the
//! worker-alive probe under `refresh.queue.lock`, the detached spawn, and
//! the marker read/clear -- so a native `post_bash_graph` and a Python one
//! leave the same store state and speak to the same worker.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// `adapters.GIT_TREE_REWRITE`, verbatim: the Bash commands that rewrite the
/// git working tree and so invalidate the whole graph.
pub const GIT_TREE_REWRITE: &str =
    r"git\s+(checkout|switch|merge|rebase|stash|pull|reset|cherry-pick|revert|apply|am)\b";

/// `crg_refresh_state.FULL_UPDATE`: the `*` marker line meaning "whole-tree
/// `code-review-graph update`, not a file list".
pub const FULL_UPDATE: &str = "*";

/// `shutil.which("code-review-graph")`: the first PATH entry holding an
/// executable file of that name.
fn which_code_review_graph() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("code-review-graph");
        if candidate.is_file() {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = candidate.metadata() {
                if meta.permissions().mode() & 0o111 != 0 {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// True when `command` rewrites the git working tree (`GIT_TREE_REWRITE`).
pub fn git_tree_rewrite_matches(command: &str) -> bool {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN
        .get_or_init(|| regex::Regex::new(GIT_TREE_REWRITE).expect("constant pattern compiles"))
        .is_match(command)
}

/// `crg_refresh_state.store_for`: `CRG_DATA_DIR` when set, else
/// `<repo>/.code-review-graph`.
fn store_dir(repo_root: &Path) -> PathBuf {
    match std::env::var("CRG_DATA_DIR") {
        Ok(raw) if !raw.trim().is_empty() => expand_home(PathBuf::from(raw.trim())),
        _ => repo_root.join(".code-review-graph"),
    }
}

/// `Path.expanduser()` for the one leading-`~` form `CRG_DATA_DIR` uses.
fn expand_home(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if text == "~" {
        return std::env::var_os("HOME").map(PathBuf::from).unwrap_or(path);
    }
    if let Some(rest) = text.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path
}

/// `store_for`'s mkdir+resolve: the store directory exists and is canonical
/// before any marker is written, exactly as the Python helper guarantees.
fn ensure_store(repo_root: &Path) -> PathBuf {
    let dir = store_dir(repo_root);
    if std::fs::create_dir_all(&dir).is_ok() {
        if let Ok(canonical) = std::fs::canonicalize(&dir) {
            return canonical;
        }
    }
    dir
}

fn marker_path(store: &Path, name: &str) -> PathBuf {
    store.join(name)
}

/// `crg_refresh_state._flock`'s opener: `os.open(path, O_RDWR | O_CREAT,
/// 0o644)` -- no truncation, ever, because these files exist to be flock
/// targets, not to be read or rewritten through this handle. The `read`+
/// `write` pair is what `O_RDWR` means; clippy's truncate heuristic cannot
/// know that, hence the targeted allow.
#[allow(clippy::suspicious_open_options)]
fn open_flock_target(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
}

/// `crg_refresh_state.request`: append `paths` to `refresh.pending` under
/// `refresh.queue.lock`, then spawn one detached worker if none holds
/// `refresh.lock`. Returns `"spawned"` or `"queued"` (Python's return values,
/// kept for the parity corpus). I/O failures propagate like Python's OSError.
pub fn request(
    store: &Path,
    paths: &[&str],
    worker_argv: &[String],
    cwd: &Path,
) -> std::io::Result<String> {
    let queue_lock = open_flock_target(&marker_path(store, "refresh.queue.lock"))?;
    flock_exclusive(&queue_lock, false)?;
    let outcome = (|| {
        let mut pending = OpenOptions::new()
            .create(true)
            .append(true)
            .open(marker_path(store, "refresh.pending"))?;
        for path in paths {
            writeln!(pending, "{path}")?;
        }
        if worker_alive(store)? {
            return Ok("queued".to_string());
        }
        spawn_worker(store, worker_argv, cwd)?;
        Ok("spawned".to_string())
    })();
    flock_unlock(&queue_lock);
    outcome
}

/// `worker_alive`: true while some process holds the worker lock.
fn worker_alive(store: &Path) -> std::io::Result<bool> {
    let lock = open_flock_target(&marker_path(store, "refresh.lock"))?;
    Ok(!flock_exclusive(&lock, true)?)
}

/// `_spawn`: the detached worker -- new session, stdin/stdout dropped,
/// stderr appended to `refresh.log`, in `cwd`. A spawn failure is the
/// caller's `request` outcome, not a hook failure: Python's `Popen` raises
/// the same way.
fn spawn_worker(store: &Path, worker_argv: &[String], cwd: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(marker_path(store, "refresh.log"))?;
    Command::new(&worker_argv[0])
        .args(&worker_argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .current_dir(cwd)
        .process_group(0) // start_new_session: survive the hook's exit
        .spawn()?;
    Ok(())
}

/// `crg_refresh_state.worker_command` + `dispatch.paths.body_path`:
/// `[interpreter, <checkout's crg_graph_refresh.py>, "--worker", "--repo",
/// <root>]`. `sys.executable` from the dispatcher's seat is the interpreter
/// forge would have exec'd (`interpreter::resolve_python`), and the body
/// lives at the project copy of `tooling/hooks/agent/` when one exists, else
/// inside the package tree under `src/`.
pub fn worker_command(repo_root: &Path) -> Vec<String> {
    let python = crate::interpreter::resolve_python(repo_root);
    let body = body_path(repo_root, "tooling/hooks/agent/crg_graph_refresh.py");
    vec![
        python.to_string_lossy().into_owned(),
        body.to_string_lossy().into_owned(),
        "--worker".to_string(),
        "--repo".to_string(),
        repo_root.to_string_lossy().into_owned(),
    ]
}

/// `dispatch.paths.body_path`: the project's copy of `relative` when one
/// exists (the monorepo layout), else the package tree's under `src/`.
pub fn body_path(root: &Path, relative: &str) -> PathBuf {
    let project = root.join(relative);
    if project.is_file() {
        return project;
    }
    root.join("src").join(relative)
}

/// `_queue([FULL_UPDATE])`: the warning when `code-review-graph` is not
/// installed, else the queue append (empty string = no warning).
pub fn queue_full_update(repo_root: &Path) -> std::io::Result<String> {
    if which_code_review_graph().is_none() {
        return Ok(format!(
            "WARNING: code-review-graph is not installed; graph NOT refreshed for {}",
            FULL_UPDATE
        ));
    }
    request(
        &ensure_store(repo_root),
        &[FULL_UPDATE],
        &worker_command(repo_root),
        repo_root,
    )?;
    Ok(String::new())
}

/// `crg_graph_refresh.full_update_output`: the PostToolUse answer after a git
/// working-tree change -- the queue warning when there is one, else the
/// "queued" line. Purely advisory: the refresh is the worker's job.
pub fn full_update_output(repo_root: &Path) -> std::io::Result<Value> {
    let warning = queue_full_update(repo_root)?;
    let context = if warning.is_empty() {
        "code-review-graph refresh queued after a git working-tree change.".to_string()
    } else {
        warning
    };
    Ok(json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": context,
        }
    }))
}

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::io::AsRawFd;

fn flock_exclusive(file: &std::fs::File, nonblocking: bool) -> std::io::Result<bool> {
    let mode = if nonblocking {
        libc::LOCK_EX | libc::LOCK_NB
    } else {
        libc::LOCK_EX
    };
    let rc = unsafe { libc::flock(file.as_raw_fd(), mode) };
    if rc == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if nonblocking && err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Ok(false); // held elsewhere: an answer, not a failure
    }
    Err(err)
}

fn flock_unlock(file: &std::fs::File) {
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
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
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = ScratchDir::new("no-marker");
        assert_eq!(failure_output("PreToolUse", tmp.path()), Value::Null);
    }

    #[test]
    fn failure_notice_reports_and_clears() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    /// A sleeper standing in for the worker: spawning it exercises the real
    /// detach (process group, refresh.log stderr) without running any
    /// refresh, and holding `refresh.lock` lets the queued-vs-spawned branch
    /// be pinned deterministically.
    fn sleeper_argv() -> Vec<String> {
        vec!["/bin/sleep".to_string(), "30".to_string()]
    }

    #[test]
    fn git_tree_rewrite_matches_the_python_regex_surface() {
        assert!(git_tree_rewrite_matches("git pull --rebase"));
        assert!(git_tree_rewrite_matches("cd /tmp && git\tcheckout -b wip"));
        assert!(git_tree_rewrite_matches("GIT_PAGER=cat git stash pop"));
        // `search`, not `fullmatch`: a rewrite verb anywhere in the command
        // counts -- including inside a quoted argument (Python agrees).
        assert!(git_tree_rewrite_matches("git commit -m 'after git pull'"));
        assert!(!git_tree_rewrite_matches("git commit -m 'bump version'"));
        assert!(!git_tree_rewrite_matches("gitx checkout"));
        assert!(!git_tree_rewrite_matches("git status"));
        assert!(!git_tree_rewrite_matches("echo gitg"));
    }

    #[test]
    fn request_appends_the_marker_and_spawns_when_no_worker_holds_the_lock() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = ScratchDir::new("request");
        let store = tmp.path().join(".code-review-graph");
        fs::create_dir_all(&store).unwrap();
        let outcome = request(&store, &[FULL_UPDATE], &sleeper_argv(), tmp.path()).unwrap();
        assert_eq!(outcome, "spawned");
        assert_eq!(
            fs::read_to_string(store.join("refresh.pending")).unwrap(),
            "*\n"
        );
        // The detached worker's stderr file exists (spawn really happened).
        assert!(store.join("refresh.log").is_file());
        // Hold refresh.lock the way a live worker would (the sleeper does not
        // take it itself): a second request must then queue, not spawn.
        let worker_lock = open_flock_target(&store.join("refresh.lock")).unwrap();
        assert!(flock_exclusive(&worker_lock, true).unwrap());
        let second = request(&store, &[FULL_UPDATE], &sleeper_argv(), tmp.path()).unwrap();
        assert_eq!(second, "queued");
        assert_eq!(
            fs::read_to_string(store.join("refresh.pending")).unwrap(),
            "*\n*\n"
        );
        flock_unlock(&worker_lock);
    }

    #[test]
    fn full_update_output_warns_when_the_tool_is_absent() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = ScratchDir::new("no-tool");
        // A PATH with exactly one writable directory and no
        // code-review-graph in it: `which` cannot find the tool.
        let empty_bin = tmp.path().join("bin");
        fs::create_dir_all(&empty_bin).unwrap();
        std::env::set_var("PATH", empty_bin.display().to_string());
        let out = full_update_output(tmp.path()).unwrap();
        std::env::remove_var("PATH");
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert_eq!(
            context,
            "WARNING: code-review-graph is not installed; graph NOT refreshed for *"
        );
        // Nothing was queued while the tool is absent.
        assert!(!tmp.path().join(".code-review-graph").exists());
    }
}
