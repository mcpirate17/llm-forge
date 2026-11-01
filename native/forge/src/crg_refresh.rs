//! Native durable graph refresh queue and hook notices. The bounded supervisor
//! rebuilds `.forge/graph.db` through this same binary, reusing cached syntax
//! facts. No interpreter, embedding provider or external graph executable starts.
//! Marker names retain compatibility with existing hook state; the external
//! `.code-review-graph/graph.db` itself is never modified.

use std::path::{Path, PathBuf};
#[path = "crg_refresh_watch.rs"]
mod watch;
#[path = "crg_refresh_worker.rs"]
mod worker;
pub fn run_worker(host: &Path, inherited: Option<i32>) -> anyhow::Result<u8> {
    worker::run(host, inherited)
}

use serde_json::{json, Value};

/// `adapters.GIT_TREE_REWRITE`, verbatim: the Bash commands that rewrite the
/// git working tree and so invalidate the whole graph.
pub const GIT_TREE_REWRITE: &str =
    r"git\s+(checkout|switch|merge|rebase|stash|pull|reset|cherry-pick|revert|apply|am)\b";

/// Full native syntax-tree refresh; unchanged file facts are reused.
pub const FULL_UPDATE: &str = "*";

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
fn ensure_store(repo_root: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let dir = store_dir(repo_root);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let meta = std::fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
        return Err(std::io::Error::other(
            "graph refresh store must be an owned, non-writable-by-others directory",
        ));
    }
    dir.canonicalize()
}

fn marker_path(store: &Path, name: &str) -> PathBuf {
    store.join(name)
}

/// Open an owned regular state file without following a final symlink. Lock
/// inodes are never truncated or replaced. Only explicit data writes truncate.
fn open_flock_target(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.nlink() != 1
        || meta.mode() & 0o022 != 0
    {
        return Err(std::io::Error::other("invalid graph refresh file"));
    }
    Ok(file)
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
        use std::io::{Seek, SeekFrom};
        if paths.len() > 4096
            || paths
                .iter()
                .any(|p| p.len() > 4096 || p.contains(['\n', '\r', '\0']))
        {
            return Err(std::io::Error::other("invalid graph refresh path bounds"));
        }
        let mut pending = open_flock_target(&marker_path(store, "refresh.pending"))?;
        let addition: usize = paths.iter().map(|p| p.len() + 1).sum();
        if pending.metadata()?.len() + addition as u64 > 262144 {
            // Every native batch rescans the whole source tree, reusing unchanged
            // facts. Collapse a flood to one full-update marker without losing work.
            pending.set_len(0)?;
            writeln!(pending, "{FULL_UPDATE}")?;
        } else {
            pending.seek(SeekFrom::End(0))?;
            for path in paths {
                writeln!(pending, "{path}")?;
            }
        }
        pending.sync_all()?;
        std::fs::File::open(store)?.sync_all()?;
        let worker_lock = open_flock_target(&marker_path(store, "refresh.lock"))?;
        if !flock_exclusive(&worker_lock, true)? {
            return Ok("queued".to_string());
        }
        spawn_worker(store, worker_argv, cwd, &worker_lock)?;
        Ok("spawned".to_string())
    })();
    flock_unlock(&queue_lock);
    outcome
}

/// `worker_alive`: true while some process holds the worker lock.
fn worker_alive(store: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    // Read-only probes do not emit IN_CLOSE_WRITE and wake their own watcher.
    let lock = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(marker_path(store, "refresh.lock"))
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let meta = lock.metadata()?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.nlink() != 1
        || meta.mode() & 0o022 != 0
    {
        return Err(std::io::Error::other("invalid graph worker lock"));
    }
    Ok(!flock_exclusive(&lock, true)?)
}

/// One detached native worker, inheriting its already-held lock. A failed spawn
/// leaves pending work durable and returns an error to the caller.
fn spawn_worker(
    store: &Path,
    worker_argv: &[String],
    cwd: &Path,
    lock: &std::fs::File,
) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    if worker_argv.is_empty() {
        return Err(std::io::Error::other("missing native refresh worker"));
    }
    let log = open_flock_target(&marker_path(store, "refresh.log"))?;
    log.set_len(0)?;
    let fd = lock.as_raw_fd();
    let mut command = Command::new(&worker_argv[0]);
    command
        .args(&worker_argv[1..])
        .arg("--lock-fd")
        .arg(fd.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .env("CRG_DATA_DIR", store)
        .current_dir(cwd);
    // SAFETY: only async-signal-safe syscalls run in the child. Inherit precisely
    // the already-held worker lock so concurrent hooks never spawn duplicates.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 || libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()?;
    Ok(())
}

/// Resolve this native executable independently of PATH and installed wrappers.
pub fn worker_command(repo_root: &Path) -> std::io::Result<Vec<String>> {
    Ok(vec![
        std::env::current_exe()?.to_string_lossy().into_owned(),
        "graph-refresh-worker".to_string(),
        "--host".to_string(),
        repo_root.to_string_lossy().into_owned(),
    ])
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
    ".py", ".rs", ".c", ".cc", ".cpp", ".cxx", ".h", ".hpp", ".js", ".jsx", ".mjs", ".ts", ".tsx",
];

/// `crg_gate._target_paths`: the write targets a payload names -- the five
/// path keys of `tool_input`, plus every `*** Add/Update/Delete File:` header
/// inside a patch-shaped string field.
pub(crate) fn target_paths(payload: &Value) -> Vec<String> {
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
pub(crate) fn resolve_like_python(path: &Path) -> PathBuf {
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

/// Source targets including deletions; directories and prose do not need refresh.
pub fn graph_files(payload: &Value, repo_root: &Path) -> Vec<String> {
    repo_relative_targets(payload, repo_root)
        .into_iter()
        .filter(|target| {
            let suffix = Path::new(target)
                .extension()
                .map(|ext| format!(".{}", ext.to_string_lossy()));
            suffix.is_some_and(|suffix| GRAPH_SUFFIXES.contains(&suffix.as_str()))
                && !repo_root.join(target).is_dir()
        })
        .collect()
}

/// Queue source changes for this binary's native indexer.
fn queue_paths(repo_root: &Path, paths: &[String]) -> std::io::Result<String> {
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    request(
        &ensure_store(repo_root)?,
        &refs,
        &worker_command(repo_root)?,
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
        "Native syntax graph refresh queued after a git working-tree change.".to_string()
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
fn consume_notice_file(store: &Path) -> anyhow::Result<Option<String>> {
    let failed_path = store.join("refresh.failed");
    match std::fs::symlink_metadata(&failed_path) {
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let queue = open_flock_target(&store.join("refresh.queue.lock"))?;
    flock_exclusive(&queue, false)?;
    match std::fs::symlink_metadata(&failed_path) {
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let text = String::from_utf8(forge::bounded_file::read(&failed_path, 65536)?)?;
    std::fs::remove_file(&failed_path)?;
    std::fs::File::open(store)?.sync_all()?;
    Ok(Some(text))
}

fn take_notices(store: &Path) -> Vec<Notice> {
    let text = match consume_notice_file(store) {
        Ok(Some(text)) => text,
        Ok(None) => return Vec::new(),
        Err(error) => {
            return vec![Notice {
                kind: "failure".into(),
                text: format!("cannot consume graph refresh notices: {error:#}"),
            }]
        }
    };
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
             `forge graph refresh` succeeds.",
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

/// Persistent failure state survives consumption of the one-shot notice.
pub fn stale_reason(repo_root: &Path) -> std::io::Result<Option<String>> {
    let path = store_dir(repo_root).join("refresh.stale");
    match std::fs::symlink_metadata(&path) {
        Ok(_) => forge::bounded_file::read(&path, 8192)
            .map_err(|e| std::io::Error::other(format!("{e:#}")))
            .and_then(|bytes| {
                String::from_utf8(bytes)
                    .map(Some)
                    .map_err(std::io::Error::other)
            }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn ensure_native_fresh(repo_root: &Path) -> anyhow::Result<()> {
    let store = store_dir(repo_root);
    if ![
        "refresh.pending",
        "refresh.inflight",
        "refresh.stale",
        "refresh.lock",
    ]
    .iter()
    .any(|name| store.join(name).exists())
    {
        return Ok(());
    }
    let waiting = wait_output(repo_root, std::time::Duration::from_secs(3))?;
    anyhow::ensure!(
        waiting.is_null(),
        "native graph refresh is still running; retry query"
    );
    if let Some(reason) = stale_reason(repo_root)? {
        anyhow::bail!("native graph is stale: {reason}; run `forge graph refresh`");
    }
    Ok(())
}

/// `crg_refresh_state.wait_for_fresh`: a graph query waits until neither a
/// pending marker nor a worker lock remains. An orphaned pending marker gets
/// one respawn attempt; a live worker is never interrupted. The default hook
/// limit is three seconds, polled every 20 ms, matching the Python store.
fn wait_for_fresh<F>(
    store: &Path,
    timeout: std::time::Duration,
    mut respawn: F,
) -> std::io::Result<bool>
where
    F: FnMut() -> std::io::Result<()>,
{
    let deadline = std::time::Instant::now() + timeout;
    let mut watch = watch::Directory::new(store)?;
    let mut respawned = false;
    loop {
        let pending = marker_path(store, "refresh.pending").exists()
            || marker_path(store, "refresh.inflight").exists();
        let alive = worker_alive(store)?;
        if !pending && !alive {
            return Ok(true);
        }
        if pending && !alive && !respawned {
            respawn()?;
            respawned = true;
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        watch.wait(deadline)?;
    }
}

pub fn wait_output(repo_root: &Path, timeout: std::time::Duration) -> std::io::Result<Value> {
    let store = ensure_store(repo_root)?;
    let fresh = wait_for_fresh(&store, timeout, || {
        request(&store, &[], &worker_command(repo_root)?, repo_root)?;
        Ok(())
    })?;
    if fresh {
        Ok(Value::Null)
    } else {
        let message = format!(
            "WARNING: a code-review-graph refresh was still running after {:.0}s; \
                 this query may read a STALE graph.",
            timeout.as_secs_f64()
        );
        Ok(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "additionalContext": message,
                },
                "systemMessage": message,
        }))
    }
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
    fn wait_times_out_while_worker_holds_the_lock() {
        use std::os::fd::AsRawFd;
        let tmp = ScratchDir::new("wait-held");
        let lock = open_flock_target(&tmp.path().join("refresh.lock")).unwrap();
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
        let started = std::time::Instant::now();
        let fresh =
            wait_for_fresh(tmp.path(), std::time::Duration::from_millis(35), || Ok(())).unwrap();
        assert!(!fresh);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn wait_respawns_once_then_returns_fresh_when_pending_clears() {
        let tmp = ScratchDir::new("wait-orphan");
        let pending = tmp.path().join("refresh.pending");
        fs::write(&pending, "a.py\n").unwrap();
        let mut calls = 0;
        let fresh = wait_for_fresh(tmp.path(), std::time::Duration::from_millis(100), || {
            calls += 1;
            fs::remove_file(&pending)
        })
        .unwrap();
        assert!(fresh);
        assert_eq!(calls, 1);
    }

    #[test]
    fn wait_output_warns_on_timeout_and_is_quiet_after_worker_exits() {
        use std::os::fd::AsRawFd;
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = ScratchDir::new("wait-output");
        let store = tmp.path().join("store");
        fs::create_dir_all(&store).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&store, fs::Permissions::from_mode(0o700)).unwrap();
        let saved = std::env::var_os("CRG_DATA_DIR");
        std::env::set_var("CRG_DATA_DIR", &store);
        let lock = open_flock_target(&store.join("refresh.lock")).unwrap();
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
        let warning = wait_output(tmp.path(), std::time::Duration::from_millis(40)).unwrap();
        assert_eq!(warning["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert!(warning["systemMessage"].as_str().unwrap().contains("STALE"));
        drop(lock);
        assert_eq!(
            wait_output(tmp.path(), std::time::Duration::from_millis(40)).unwrap(),
            Value::Null
        );
        if let Some(value) = saved {
            std::env::set_var("CRG_DATA_DIR", value);
        } else {
            std::env::remove_var("CRG_DATA_DIR");
        }
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
    fn requests_coalesce_without_spawning_while_worker_owns_lock() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = ScratchDir::new("request");
        let store = tmp.path().join(".code-review-graph");
        fs::create_dir_all(&store).unwrap();
        let worker_lock = open_flock_target(&store.join("refresh.lock")).unwrap();
        assert!(flock_exclusive(&worker_lock, true).unwrap());
        for _ in 0..2 {
            assert_eq!(
                request(&store, &[FULL_UPDATE], &[], tmp.path()).unwrap(),
                "queued"
            );
        }
        assert_eq!(
            fs::read_to_string(store.join("refresh.pending")).unwrap(),
            "*\n*\n"
        );
        flock_unlock(&worker_lock);
        assert!(!store.join("refresh.log").exists());
    }

    #[test]
    fn full_update_queues_without_an_external_graph_executable() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = ScratchDir::new("no-tool");
        let store = ensure_store(tmp.path()).unwrap();
        let worker_lock = open_flock_target(&store.join("refresh.lock")).unwrap();
        assert!(flock_exclusive(&worker_lock, true).unwrap());
        let out = full_update_output(tmp.path()).unwrap();
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert_eq!(
            context,
            "Native syntax graph refresh queued after a git working-tree change."
        );
        assert_eq!(
            fs::read_to_string(store.join("refresh.pending")).unwrap(),
            "*\n"
        );
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
        // Deletions must refresh the graph too; prose remains irrelevant.
        assert!(graph_files(&json!({"tool_input": {"file_path": "b.md"}}), &root).is_empty());
        assert_eq!(
            graph_files(&json!({"tool_input": {"file_path": "ghost.py"}}), &root),
            vec!["ghost.py"]
        );
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
    fn edits_and_deletions_queue_native_refresh_while_prose_is_quiet() {
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
        let store = ensure_store(&root).unwrap();
        let worker_lock = open_flock_target(&store.join("refresh.lock")).unwrap();
        assert!(flock_exclusive(&worker_lock, true).unwrap());
        let out = edit_hook_output(&json!({"tool_input": {"file_path": "a.py"}}), &root).unwrap();
        assert_eq!(out, quiet);
        assert_eq!(
            fs::read_to_string(store.join("refresh.pending")).unwrap(),
            "a.py\n"
        );
    }
}
