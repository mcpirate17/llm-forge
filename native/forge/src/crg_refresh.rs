//! Native port of `crg_graph_refresh`'s PostToolUse surface:
//! `failure_output` (via `crg_refresh_state.take_notices`) for
//! `crg_refresh_report_post`, `_queue([FULL_UPDATE])` /
//! `full_update_output` for `post_bash_graph` -- queue a whole-tree refresh
//! by appending to the graph store's `refresh.pending` marker and spawning
//! one detached worker when none is running, then return at once -- and
//! `hook_output` (`edit_hook_output`) for the `crg_graph_refresh` hook
//! itself: PostToolUse on Edit/Write/NotebookEdit queues the edited
//! graph-suffix files the same way and never waits.
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

/// `_queue([FULL_UPDATE])`: the whole-tree queue append (empty string = no
/// warning).
pub fn queue_full_update(repo_root: &Path) -> std::io::Result<String> {
    queue_paths(repo_root, &[FULL_UPDATE.to_string()])
}

/// `crg_gate.REPO_ROOT`'s ladder: `CRG_GATE_REPO_ROOT` (tests), then
/// `PROJECT_DIR` (the exec launcher), then the session checkout. Python's
/// fourth rung is the module file's own location; forge has no module file,
/// so the dispatcher's checkout (`interpreter::project_root()`) is the
/// authority -- the same root every other native hook resolves against.
/// Canonicalized like Python's outer `.resolve()`, so `strip_prefix` against
/// a canonicalized target agrees.
pub fn gate_repo_root() -> PathBuf {
    let raw = ["CRG_GATE_REPO_ROOT", "PROJECT_DIR"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(crate::interpreter::project_root);
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

/// `crg_graph_refresh.GRAPH_SUFFIXES`: the suffixes whose edits touch the
/// graph (prose and config files do not).
pub const GRAPH_SUFFIXES: &[&str] = &[
    ".py", ".rs", ".c", ".cc", ".cpp", ".cxx", ".h", ".hpp", ".sh", ".bash",
];

/// `crg_gate._target_paths`: the write targets a payload names -- the five
/// path keys of `tool_input`, plus every `*** Add/Update/Delete File:` header
/// inside a patch-shaped string field.
fn target_paths(payload: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    let empty = serde_json::Map::new();
    let tool_input = payload
        .get("tool_input")
        .or_else(|| payload.get("toolInput"))
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    for key in [
        "file_path",
        "filePath",
        "path",
        "notebook_path",
        "target_file",
    ] {
        if let Some(value) = tool_input.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                paths.push(value.to_string());
            }
        }
    }
    static PATCH_FILE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATCH_FILE.get_or_init(|| {
        // `(?m)`: Python compiles this with re.MULTILINE, so every line of a
        // multi-line patch is a candidate, not just the whole-string head.
        regex::Regex::new(r"(?m)^\*\*\* (?:Add|Update|Delete) File: (.+)$")
            .expect("constant pattern compiles")
    });
    for source in [
        tool_input.get("patch"),
        tool_input.get("input"),
        tool_input.get("command"),
        payload.get("patch"),
        payload.get("input"),
        payload.get("command"),
    ] {
        let Some(text) = source.and_then(Value::as_str) else {
            continue;
        };
        for capture in pattern.captures_iter(text) {
            paths.push(capture[1].trim().to_string());
        }
    }
    paths
}

/// Python's non-strict `Path.resolve()` for the shapes a hook target takes:
/// an existing path canonicalizes; a missing leaf resolves against its
/// canonical parent; only when even the parent is missing does the path stay
/// lexical (normalized `.`/`..` components, never above the root).
fn resolve_like_python(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    if let Some(parent) = path.parent() {
        if let Ok(canonical_parent) = std::fs::canonicalize(parent) {
            return match path.file_name() {
                Some(name) => canonical_parent.join(name),
                None => canonical_parent,
            };
        }
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::Prefix(_) => {}
        }
    }
    normalized
}

/// `crg_gate._classify_targets`'s local half (`_repo_relative_targets`):
/// the targets that land inside this checkout, as checkout-relative posix
/// paths. Sibling-worktree and outside-checkout targets are dropped -- the
/// graph only refreshes what this checkout owns.
fn repo_relative_targets(payload: &Value, repo_root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for raw in target_paths(payload) {
        let target = PathBuf::from(&raw);
        let resolved = if target.is_absolute() {
            resolve_like_python(&target)
        } else {
            resolve_like_python(&repo_root.join(target))
        };
        if let Ok(relative) = resolved.strip_prefix(repo_root) {
            out.push(relative.to_string_lossy().into_owned());
        }
    }
    out
}

/// `crg_graph_refresh._graph_files`: the queued subset of the payload's
/// targets -- a known graph suffix that exists as a file under the checkout.
pub fn graph_files(payload: &Value, repo_root: &Path) -> Vec<String> {
    repo_relative_targets(payload, repo_root)
        .into_iter()
        .filter(|target| {
            let suffix = Path::new(target)
                .extension()
                .map(|ext| format!(".{}", ext.to_string_lossy()));
            suffix.is_some_and(|suffix| GRAPH_SUFFIXES.contains(&suffix.as_str()))
                && repo_root.join(target).is_file()
        })
        .collect()
}

/// `_queue(paths)`: the warning when `code-review-graph` is not installed,
/// else the queue append + detached worker spawn (empty string = no warning).
fn queue_paths(repo_root: &Path, paths: &[String]) -> std::io::Result<String> {
    if which_code_review_graph().is_none() {
        return Ok(format!(
            "WARNING: code-review-graph is not installed; graph NOT refreshed for {}",
            paths.join(", ")
        ));
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    request(
        &ensure_store(repo_root),
        &refs,
        &worker_command(repo_root),
        repo_root,
    )?;
    Ok(String::new())
}

/// `crg_graph_refresh.hook_output`: the dispatcher's PostToolUse
/// Edit/Write/NotebookEdit answer -- queue the graph-relevant targets and
/// never wait. Quiet on success (the queue owns the refresh); the
/// not-installed warning is the only context it ever adds.
pub fn edit_hook_output(payload: &Value, repo_root: &Path) -> std::io::Result<Value> {
    let files = graph_files(payload, repo_root);
    if files.is_empty() {
        return Ok(json!({
            "hookSpecificOutput": {"hookEventName": "PostToolUse"}
        }));
    }
    let warning = queue_paths(repo_root, &files)?;
    if warning.is_empty() {
        return Ok(json!({
            "hookSpecificOutput": {"hookEventName": "PostToolUse"}
        }));
    }
    Ok(json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": warning,
        }
    }))
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

    #[test]
    fn gate_repo_root_and_resolve_follow_the_python_ladder() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = ScratchDir::new("gate-root");
        let root = tmp.path().canonicalize().unwrap();
        std::env::set_var("CRG_GATE_REPO_ROOT", tmp.path());
        // Canonicalized like Python's outer `.resolve()`, so `strip_prefix`
        // against a canonicalized target agrees.
        assert_eq!(gate_repo_root(), root);
        std::env::remove_var("CRG_GATE_REPO_ROOT");
        // `resolve_like_python`: an existing path canonicalizes; a missing
        // leaf joins its canonical parent; a fully missing path stays lexical
        // (normalized, never above the root).
        assert_eq!(resolve_like_python(&root), root);
        assert_eq!(
            resolve_like_python(&root.join("ghost.py")),
            root.join("ghost.py")
        );
        assert_eq!(
            resolve_like_python(Path::new("/nonexistent-xyz-123/../../../x.py")),
            PathBuf::from("/x.py")
        );
    }

    #[test]
    fn graph_files_keeps_existing_graph_suffix_targets() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = ScratchDir::new("graph-files");
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join("a.py"), "x = 1\n").unwrap();
        fs::write(root.join("b.md"), "prose\n").unwrap();
        fs::write(root.join("sub.rs"), "fn f() {}\n").unwrap();
        assert_eq!(
            graph_files(&json!({"tool_input": {"file_path": "a.py"}}), &root),
            vec!["a.py".to_string()]
        );
        // Prose suffixes and missing files are dropped even when named.
        assert!(graph_files(&json!({"tool_input": {"file_path": "b.md"}}), &root).is_empty());
        assert!(graph_files(&json!({"tool_input": {"file_path": "ghost.py"}}), &root).is_empty());
        // An absolute path inside the checkout is repo-relative first.
        assert_eq!(
            graph_files(
                &json!({"tool_input": {"file_path": root.join("sub.rs").display().to_string()}}),
                &root
            ),
            vec!["sub.rs".to_string()]
        );
        // A patch-shaped command names its targets too, and a path outside
        // the checkout is never this checkout's to refresh.
        assert_eq!(
            graph_files(
                &json!({"tool_input": {"command": "*** Add File: a.py\n+new\n*** Update File: /elsewhere/x.py"}}),
                &root
            ),
            vec!["a.py".to_string()]
        );
    }

    #[test]
    fn edit_hook_output_is_quiet_or_warns_like_the_python_body() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = ScratchDir::new("edit-hook");
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join("a.py"), "x = 1\n").unwrap();
        // No graph-relevant target named: quiet, and nothing queued.
        let quiet = json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}});
        assert_eq!(
            edit_hook_output(&json!({"tool_input": {"file_path": "b.md"}}), &root).unwrap(),
            quiet
        );
        // Tool absent: the not-installed warning is the only context the
        // edit path ever adds, and nothing is queued behind it.
        let empty_bin = tmp.path().join("bin");
        fs::create_dir_all(&empty_bin).unwrap();
        std::env::set_var("PATH", empty_bin.display().to_string());
        let out = edit_hook_output(&json!({"tool_input": {"file_path": "a.py"}}), &root).unwrap();
        std::env::remove_var("PATH");
        assert_eq!(
            out["hookSpecificOutput"]["additionalContext"],
            json!("WARNING: code-review-graph is not installed; graph NOT refreshed for a.py")
        );
        assert!(!root.join(".code-review-graph").exists());
    }
}
