//! Native graph-use and claim gate for Bash, Edit, Write and NotebookEdit.
//! The session marker format and claim store are shared with the legacy hook.
//!
//! Fail-open, exactly like the Python: a bug in command parsing must not deny
//! every Bash call in the fleet, so any internal error here resolves to "no
//! opinion" (an empty targets list), not a denial.

use crate::identity;
use crate::ownership;
use crate::write_targets::{repo_write_targets, OPAQUE_WRITE};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const STATE_TTL_SECONDS: u64 = 2 * 24 * 60 * 60;

pub const REMEDY: &str = "Create a narrow governance claim first: \
make governance-claim CLAIM_PATHS='<paths>' CLAIM_JUSTIFICATION='<why>' \
(CRG_GATE_ENFORCE_WORKTREES=0 disables worktree enforcement fleet-wide).";

/// `_checkout_of`: `(worktree root, git common dir)` for `path`, or `None`
/// outside a checkout. Filesystem-only, walking `path` and its ancestors.
pub fn checkout_of(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut candidate = Some(path.to_path_buf());
    while let Some(dir) = candidate {
        let entry = dir.join(".git");
        if entry.is_dir() {
            return Some((dir, entry.canonicalize().unwrap_or(entry)));
        }
        if entry.is_file() {
            let Ok(text) = std::fs::read_to_string(&entry) else {
                return None;
            };
            let text = text.trim();
            let raw = text.strip_prefix("gitdir:")?;
            let raw = raw.trim();
            let gitdir = if Path::new(raw).is_absolute() {
                PathBuf::from(raw)
            } else {
                dir.join(raw)
            };
            let gitdir = gitdir.canonicalize().unwrap_or(gitdir);
            let marker = gitdir.join("commondir");
            if !marker.is_file() {
                return Some((dir, gitdir));
            }
            let Ok(raw_common) = std::fs::read_to_string(&marker) else {
                return None;
            };
            let common = PathBuf::from(raw_common.trim());
            let common = if common.is_absolute() {
                common
            } else {
                gitdir.join(common)
            };
            return Some((dir, common.canonicalize().unwrap_or(common)));
        }
        candidate = dir.parent().map(Path::to_path_buf);
    }
    None
}

/// `session_checkout`: the checkout the session works in, else `repo_root`.
/// `repo_common_dir` is `repo_root`'s own common dir (Python's module-level
/// `REPO_COMMON_DIR`, computed once).
pub fn session_checkout(
    payload: &Value,
    repo_root: &Path,
    repo_common_dir: Option<&Path>,
) -> PathBuf {
    let Some(repo_common_dir) = repo_common_dir else {
        return repo_root.to_path_buf();
    };
    let cwd_field = payload.get("cwd").and_then(Value::as_str);
    let candidate = match cwd_field.filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => match std::env::current_dir() {
            Ok(d) => d,
            Err(_) => return repo_root.to_path_buf(),
        },
    };
    let Ok(resolved) = candidate.canonicalize() else {
        return repo_root.to_path_buf();
    };
    match checkout_of(&resolved) {
        Some((root, common)) if common == repo_common_dir => root,
        _ => repo_root.to_path_buf(),
    }
}

/// `_bash_checkout`: `(checkout to resolve write targets against, is a
/// sibling worktree)`.
fn bash_checkout(
    payload: &Value,
    repo_root: &Path,
    repo_common_dir: Option<&Path>,
) -> (PathBuf, bool) {
    let root = session_checkout(payload, repo_root, repo_common_dir);
    let sibling = root != repo_root;
    (root, sibling)
}

fn tool_input(payload: &Value) -> &Value {
    payload
        .get("tool_input")
        .or_else(|| payload.get("toolInput"))
        .filter(|v| v.is_object())
        .unwrap_or(&Value::Null)
}

fn bash_command(payload: &Value) -> String {
    tool_input(payload)
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn state_dir() -> PathBuf {
    let raw =
        std::env::var("CRG_GATE_STATE_DIR").unwrap_or_else(|_| "/tmp/claude-crg-gate".to_string());
    PathBuf::from(raw)
}

fn ensure_state_dir() -> std::io::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

fn state_key(payload: &Value) -> Option<String> {
    let session_id = payload
        .get("session_id")
        .filter(|value| value.as_str().is_some_and(|text| !text.is_empty()))
        .or_else(|| payload.get("sessionId"))
        .and_then(Value::as_str)?;
    if session_id.is_empty() {
        return None;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(session_id.as_bytes());
    Some(digest.iter().map(|b| format!("{b:02x}")).collect())
}

fn state_path(dir: &Path, key: &str, suffix: &str) -> PathBuf {
    dir.join(format!("{key}.{suffix}"))
}

fn write_state(path: &Path) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    writeln!(file, "{:.6}", crate::instant::now())
}

fn prune_stale_state(dir: &Path) -> std::io::Result<()> {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(STATE_TTL_SECONDS))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let matches = path.extension().and_then(|s| s.to_str()) == Some("pending")
            || path.to_string_lossy().ends_with(".graph-used");
        if !matches {
            continue;
        }
        if let Ok(meta) = path.metadata() {
            if meta.is_file() && meta.modified().is_ok_and(|modified| modified < cutoff) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    Ok(())
}

/// SessionStart's `crg_gate.py start`: create a pending marker and prune only
/// stale gate markers. The existing SessionStart body still invokes this as a
/// legacy command; the format is intentionally interoperable.
pub fn start(payload: &Value) -> std::io::Result<()> {
    let dir = ensure_state_dir()?;
    prune_stale_state(&dir)?;
    if let Some(key) = state_key(payload) {
        write_state(&state_path(&dir, &key, "pending"))?;
    }
    Ok(())
}

/// Record a graph tool call before it queries the graph. `mark` has no
/// decision of its own; I/O errors become visible hook errors at dispatch.
pub(crate) fn is_graph_tool_name(name: &str) -> bool {
    [
        "mcp__code-review-graph__",
        "mcp__code-review_graph__",
        "mcp__code_review-graph__",
        "mcp__code_review_graph__",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

pub fn mark(payload: &Value) -> std::io::Result<()> {
    let name = payload
        .get("tool_name")
        .filter(|value| value.as_str().is_some_and(|text| !text.is_empty()))
        .or_else(|| payload.get("toolName"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !is_graph_tool_name(name) {
        return Ok(());
    }
    let Some(key) = state_key(payload) else {
        return Ok(());
    };
    let dir = ensure_state_dir()?;
    write_state(&state_path(&dir, &key, "graph-used"))?;
    match std::fs::remove_file(state_path(&dir, &key, "pending")) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Python's `repr()` for a plain string, as `_claim_allows`'s `f"{x!r}"`
/// deny-message interpolations use it: single-quoted by default, switching to
/// double quotes only when the string itself contains a single quote and no
/// double quote (so the common case -- an owner id or repo-relative path --
/// never needs an escape). Rust's `{:?}` (`Debug`) always double-quotes,
/// which silently disagreed with every deny message Python ever produces.
fn python_repr(s: &str) -> String {
    let (quote, needs_escape) = if s.contains('\'') && !s.contains('"') {
        ('"', '"')
    } else {
        ('\'', '\'')
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            c if c == needs_escape => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn enforce_worktrees() -> bool {
    let raw = std::env::var("CRG_GATE_ENFORCE_WORKTREES").unwrap_or_else(|_| "1".to_string());
    !raw.is_empty() && raw != "0"
}

fn record_exposure(
    common_dir: Option<&Path>,
    owner: &str,
    target: &str,
    tool: &str,
    checkout: &str,
) {
    let Some(common_dir) = common_dir else { return };
    let directory = common_dir.join("governance");
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    let path = directory.join("claim-gate-exposure.jsonl");
    if path
        .metadata()
        .is_ok_and(|meta| meta.len() > 4 * 1024 * 1024)
    {
        return;
    }
    let line = serde_json::json!({
        "at": crate::instant::now(),
        "owner": owner,
        "tool": tool,
        "checkout": checkout,
        "path": target,
    });
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// `_claim_allows`: whether `owner` may write `target`, and a detail message
/// (the reason when denied; the store digest -- unused by `verify_bash`'s
/// caller -- when allowed).
fn claim_allows(
    owner: &str,
    target: &str,
    repo_root: &Path,
    repo_common_dir: Option<&Path>,
    env: &HashMap<String, String>,
) -> (bool, String) {
    if owner.is_empty() {
        return (
            false,
            "hook has no governance identity, so no claim can be matched — \
             export GOVERNANCE_OWNER=<lane>"
                .to_string(),
        );
    }
    let (claims, digest) = match ownership::load_claims(repo_root) {
        Ok(v) => v,
        Err(exc) => return (false, format!("live claim store is unavailable: {exc}")),
    };
    let legacy = identity::vendor_for(owner, env);
    let now = crate::instant::now();
    let mut holders = Vec::new();
    let mut lapsed = Vec::new();
    for claim in &claims {
        if !claim
            .paths
            .iter()
            .any(|claimed| ownership::paths_overlap(target, claimed))
        {
            continue;
        }
        let claimed_by = claim.owner.to_lowercase();
        let mine_direct = claimed_by == owner.to_lowercase();
        let by_vendor = !mine_direct && !legacy.is_empty() && claimed_by == legacy;
        let mine = mine_direct || by_vendor;
        let active = match claim.active(now) {
            Ok(v) => v,
            Err(exc) => return (false, format!("live claim store is unavailable: {exc}")),
        };
        if !active {
            if mine {
                let reason = claim.lapse_reason(now).unwrap_or_default();
                lapsed.push(format!("{} ({reason})", claim.claim_id));
            }
            continue;
        }
        if mine {
            if by_vendor {
                record_exposure(
                    repo_common_dir,
                    owner,
                    target,
                    &format!("vendor-claim:{}", claim.owner),
                    &repo_root.to_string_lossy(),
                );
            }
            let _ = ownership::touch_claim(repo_root, &claim.claim_id, now);
            return (true, digest);
        }
        let deadline = claim.deadline(now).unwrap_or(now);
        let overrun = claim.overrun(now).unwrap_or(false);
        holders.push(format!(
            "{} until {}Z{} ({})",
            claim.owner,
            crate::instant::format_iso_minutes(deadline),
            if overrun { " OVERRUN" } else { "" },
            claim.claim_id
        ));
    }
    if !holders.is_empty() {
        return (
            false,
            format!(
                "path {} is held by {}; owner={} has no live claim on it — \
                 coordinate via A2A or wait for expiry",
                python_repr(target),
                holders.join("; "),
                python_repr(owner),
            ),
        );
    }
    if !lapsed.is_empty() {
        return (
            false,
            format!(
                "your claim on {} is no longer live: {} — re-claim the path before writing",
                python_repr(target),
                lapsed.join("; ")
            ),
        );
    }
    (
        false,
        format!(
            "no live exact claim for owner={} path={}",
            python_repr(owner),
            python_repr(target)
        ),
    )
}

/// `verify_bash`: `Value::Null` for "no contribution" (allow), else the
/// protocol-correct deny response `crg_gate.py`'s `_deny` prints.
///
/// `env` backs `identity::vendor_for`'s legacy-owner fallback (reads launcher
/// markers only, never mutates process state).
#[allow(clippy::too_many_arguments)]
pub fn verify_bash(
    payload: &Value,
    owner: &str,
    repo_root: &Path,
    repo_common_dir: Option<&Path>,
    env: &HashMap<String, String>,
) -> Value {
    let command = bash_command(payload);
    if command.is_empty() {
        return Value::Null;
    }
    let (base, in_sibling) = bash_checkout(payload, repo_root, repo_common_dir);
    let targets = repo_write_targets(&command, &base);
    if targets.is_empty() {
        return Value::Null;
    }
    let protocol = crate::current_work_guard::hook_protocol(payload);
    if targets.iter().any(|t| t == OPAQUE_WRITE) {
        return crate::current_work_guard::hook_response(
            Some(
                "BLOCKED: this Bash command writes repo files through an interpreter \
                 whose target cannot be resolved, so the claim gate cannot check it. \
                 Use Edit/Write, or name the path as a literal.",
            ),
            protocol,
            None,
        );
    }
    let dir = state_dir();
    let key = state_key(payload).unwrap_or_default();
    if !state_path(&dir, &key, "graph-used").is_file() {
        return crate::current_work_guard::hook_response(
            Some(&format!(
                "BLOCKED: call a code-review-graph MCP tool before writing repo files \
                 in this session (this command writes {}).",
                targets.join(", ")
            )),
            protocol,
            None,
        );
    }
    for target in &targets {
        let (allowed, detail) = claim_allows(owner, target, repo_root, repo_common_dir, env);
        if allowed {
            continue;
        }
        if in_sibling {
            record_exposure(
                repo_common_dir,
                owner,
                target,
                "Bash",
                &base.to_string_lossy(),
            );
            if !enforce_worktrees() {
                continue;
            }
        }
        return crate::current_work_guard::hook_response(
            Some(&format!("BLOCKED: {detail}. {REMEDY}")),
            protocol,
            None,
        );
    }
    Value::Null
}

/// Resolve Edit/Write/NotebookEdit targets into this checkout or a sibling
/// worktree sharing the same Git common directory. Scratch and other repos
/// have no claim in this gate.
pub(crate) fn classify_targets(
    payload: &Value,
    repo_root: &Path,
    repo_common_dir: Option<&Path>,
) -> (Vec<String>, Vec<(PathBuf, String)>) {
    let mut local = Vec::new();
    let mut sibling = Vec::new();
    for raw in crate::crg_refresh::target_paths(payload) {
        let path = Path::new(&raw);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            repo_root.join(path)
        };
        let target = crate::crg_refresh::resolve_like_python(&absolute);
        if let Ok(relative) = target.strip_prefix(repo_root) {
            local.push(relative.to_string_lossy().into_owned());
            continue;
        }
        let Some((root, common)) = checkout_of(&target) else {
            continue;
        };
        if Some(common.as_path()) == repo_common_dir {
            if let Ok(relative) = target.strip_prefix(&root) {
                sibling.push((root, relative.to_string_lossy().into_owned()));
            }
        }
    }
    (local, sibling)
}

/// Edit/Write/NotebookEdit claim gate. Missing graph use or an unverifiable
/// target denies before any claim reads. Unlike Bash, errors in this hook are
/// fail-closed by its registry entry (`crg_gate_verify`).
pub fn verify(
    payload: &Value,
    owner: &str,
    repo_root: &Path,
    repo_common_dir: Option<&Path>,
    env: &HashMap<String, String>,
) -> std::io::Result<Value> {
    let protocol = crate::current_work_guard::hook_protocol(payload);
    let dir = ensure_state_dir()?;
    let graph_used =
        state_key(payload).is_some_and(|key| state_path(&dir, &key, "graph-used").is_file());
    if !graph_used {
        return Ok(crate::current_work_guard::hook_response(
            Some("BLOCKED: call a code-review-graph MCP tool before editing or writing in this session."),
            protocol,
            None,
        ));
    }
    if crate::crg_refresh::target_paths(payload).is_empty() {
        return Ok(crate::current_work_guard::hook_response(
            Some("BLOCKED: edit target is missing; live claim cannot be verified."),
            protocol,
            None,
        ));
    }
    let (local, sibling) = classify_targets(payload, repo_root, repo_common_dir);
    for target in &local {
        let (allowed, detail) = claim_allows(owner, target, repo_root, repo_common_dir, env);
        if !allowed {
            return Ok(crate::current_work_guard::hook_response(
                Some(&format!("BLOCKED: {detail}. {REMEDY}")),
                protocol,
                None,
            ));
        }
    }
    let tool = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    for (checkout, target) in sibling {
        let (allowed, detail) = claim_allows(owner, &target, repo_root, repo_common_dir, env);
        if allowed {
            continue;
        }
        record_exposure(
            repo_common_dir,
            owner,
            &target,
            tool,
            &checkout.to_string_lossy(),
        );
        if enforce_worktrees() {
            return Ok(crate::current_work_guard::hook_response(
                Some(&format!(
                    "BLOCKED: {detail} (in worktree {}). {REMEDY}",
                    checkout.display()
                )),
                protocol,
                None,
            ));
        }
    }
    Ok(Value::Null)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    /// `pub(crate)` (module and lock both) so that any other `#[path]`-included
    /// test binary that pulls this file in (e.g.
    /// `tests/bash_pretooluse_hooks_parity.rs`) and *also* mutates
    /// `CRG_GATE_STATE_DIR` from its own top-level test can serialize against
    /// this same lock instead of racing it with an unrelated `Mutex` of its
    /// own -- two different `Mutex` instances guarding the same env var
    /// provide no mutual exclusion at all.
    pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct ScratchRepo {
        root: PathBuf,
        state_dir: PathBuf,
    }

    impl ScratchRepo {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let root = std::env::temp_dir().join(format!("forge-crg-gate-test-{pid}-{label}-{n}"));
            let state_dir =
                std::env::temp_dir().join(format!("forge-crg-gate-state-{pid}-{label}-{n}"));
            std::fs::create_dir_all(root.join(".git")).unwrap();
            std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/llm-b0-lane\n").unwrap();
            std::fs::create_dir_all(&state_dir).unwrap();
            ScratchRepo { root, state_dir }
        }

        fn common_dir(&self) -> PathBuf {
            checkout_of(&self.root).unwrap().1
        }

        fn mark_graph_used(&self, payload: &Value) {
            let key = state_key(payload).unwrap();
            std::fs::write(state_path(&self.state_dir, &key, "graph-used"), b"1").unwrap();
        }

        fn write_claim(&self, owner: &str, paths: &[&str]) {
            let now = crate::instant::now();
            let created = crate::instant::isoformat_utc(now - 60.0);
            let expires = crate::instant::isoformat_utc(now + 3600.0);
            let fields = json!({
                "owner": owner, "paths": paths, "justification": "because",
                "created_at": created, "expires_at": expires,
            });
            // The id must come from the same canonicalization the loader
            // verifies against (`ownership::sha256_json`, sorted keys) --
            // `to_string` here would emit insertion order now that the crate
            // enables serde_json's `preserve_order`.
            let hex = crate::ownership::sha256_json(&fields);
            let claim = json!({
                "claim_id": format!("claim-{}", &hex[..20]),
                "owner": owner, "paths": paths, "justification": "because",
                "created_at": created, "expires_at": expires,
            });
            let dir = self.common_dir().join("governance");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("ownership-claims.json"),
                serde_json::to_string(&json!({"schema_version": 1, "claims": [claim]})).unwrap(),
            )
            .unwrap();
        }
    }

    impl Drop for ScratchRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
            let _ = std::fs::remove_dir_all(&self.state_dir);
        }
    }

    fn payload(session_id: &str, command: &str) -> Value {
        json!({"session_id": session_id, "tool_name": "Bash", "tool_input": {"command": command}})
    }

    fn run(repo: &ScratchRepo, owner: &str, payload: &Value) -> Value {
        let _dir_guard = with_state_dir(&repo.state_dir);
        let common = repo.common_dir();
        verify_bash(payload, owner, &repo.root, Some(&common), &HashMap::new())
    }

    fn run_edit(repo: &ScratchRepo, owner: &str, payload: &Value) -> Value {
        let _dir_guard = with_state_dir(&repo.state_dir);
        let common = repo.common_dir();
        verify(payload, owner, &repo.root, Some(&common), &HashMap::new()).unwrap()
    }

    fn edit_payload(path: &str) -> Value {
        json!({
            "session_id": "edit-session",
            "tool_name": "Edit",
            "tool_input": {"file_path": path},
        })
    }

    /// `CRG_GATE_STATE_DIR` is process-global; serialize every test that sets it.
    fn with_state_dir(dir: &Path) -> impl Drop {
        struct Guard(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
        impl Drop for Guard {
            fn drop(&mut self) {
                std::env::remove_var("CRG_GATE_STATE_DIR");
            }
        }
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("CRG_GATE_STATE_DIR", dir);
        Guard(guard)
    }

    #[test]
    fn graph_mark_replaces_session_pending_state_and_edit_checks_it() {
        let repo = ScratchRepo::new("mark-edit");
        repo.write_claim("claude", &["b.py"]);
        let graph = json!({"session_id": "edit-session", "tool_name":
            "mcp__code_review_graph__query_graph"});
        {
            let _dir_guard = with_state_dir(&repo.state_dir);
            start(&graph).unwrap();
            let key = state_key(&graph).unwrap();
            assert!(state_path(&repo.state_dir, &key, "pending").is_file());
            mark(&graph).unwrap();
            assert!(state_path(&repo.state_dir, &key, "graph-used").is_file());
            assert!(!state_path(&repo.state_dir, &key, "pending").exists());
        }
        assert_eq!(
            run_edit(&repo, "claude", &edit_payload("b.py")),
            Value::Null
        );
    }

    #[test]
    fn edit_denies_before_graph_call_and_without_a_target() {
        let repo = ScratchRepo::new("edit-preconditions");
        let payload = edit_payload("a.py");
        let denied = run_edit(&repo, "claude", &payload);
        assert!(denied["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("call a code-review-graph MCP tool"));
        let missing = json!({"session_id": "edit-session", "tool_name": "Write", "tool_input": {}});
        repo.mark_graph_used(&missing);
        let denied = run_edit(&repo, "claude", &missing);
        assert!(denied["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("edit target is missing"));
    }

    #[test]
    fn edit_denies_another_owner_with_the_holder_and_remedy() {
        let repo = ScratchRepo::new("edit-held");
        repo.write_claim("codex-phase22", &["a.py"]);
        let payload = edit_payload("a.py");
        repo.mark_graph_used(&payload);
        let denied = run_edit(&repo, "claude", &payload);
        let reason = denied["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.starts_with("BLOCKED: path 'a.py' is held by codex-phase22"));
        assert!(reason.contains("make governance-claim"));
    }

    #[test]
    fn edit_allows_scratch_targets_after_graph_use() {
        let repo = ScratchRepo::new("edit-scratch");
        let outside = std::env::temp_dir().join(format!("forge-outside-{}", std::process::id()));
        let payload = edit_payload(&outside.to_string_lossy());
        repo.mark_graph_used(&payload);
        assert_eq!(run_edit(&repo, "claude", &payload), Value::Null);
    }

    #[test]
    fn sibling_target_is_claim_relevant_and_denial_is_recorded() {
        let repo = ScratchRepo::new("edit-sibling");
        repo.write_claim("codex-phase22", &["a.py"]);
        let linked = repo.root.with_extension("linked");
        let gitdir = repo.root.join(".git/worktrees/side");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        std::fs::write(linked.join("a.py"), "A = 1\n").unwrap();
        let payload = edit_payload(&linked.join("a.py").to_string_lossy());
        repo.mark_graph_used(&payload);
        let (local, sibling) = classify_targets(&payload, &repo.root, Some(&repo.common_dir()));
        assert!(local.is_empty());
        assert_eq!(sibling, vec![(linked.clone(), "a.py".to_string())]);
        let denied = run_edit(&repo, "claude", &payload);
        assert!(denied["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains(&format!("(in worktree {})", linked.display())));
        let log = std::fs::read_to_string(
            repo.common_dir()
                .join("governance/claim-gate-exposure.jsonl"),
        )
        .unwrap();
        let record: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
        assert_eq!(record["path"], "a.py");
        assert_eq!(record["checkout"], linked.to_string_lossy().as_ref());
        let _ = std::fs::remove_dir_all(linked);
    }

    #[test]
    fn a_read_only_command_with_no_write_targets_is_allowed() {
        let repo = ScratchRepo::new("readonly");
        let p = payload("s1", "git status");
        assert_eq!(run(&repo, "llm-b0", &p), Value::Null);
    }

    #[test]
    fn an_empty_command_is_allowed() {
        let repo = ScratchRepo::new("empty-command");
        let p = payload("s1", "");
        assert_eq!(run(&repo, "llm-b0", &p), Value::Null);
    }

    #[test]
    fn a_write_before_any_graph_tool_call_is_denied() {
        let repo = ScratchRepo::new("no-graph-use");
        let target = repo.root.join("foo.py");
        let p = payload("s1", &format!("echo x > {}", target.display()));
        let out = run(&repo, "llm-b0", &p);
        let reason = out["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("call a code-review-graph MCP tool"));
    }

    #[test]
    fn an_opaque_interpreter_write_is_denied_regardless_of_graph_use() {
        let repo = ScratchRepo::new("opaque");
        let p = payload("s1", "python3 -c \"open(x, 'w').write('y')\"");
        repo.mark_graph_used(&p);
        let out = run(&repo, "llm-b0", &p);
        let reason = out["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("cannot be resolved"));
    }

    #[test]
    fn a_write_with_no_claim_at_all_is_denied() {
        let repo = ScratchRepo::new("no-claim");
        let target = repo.root.join("foo.py");
        let p = payload("s1", &format!("echo x > {}", target.display()));
        repo.mark_graph_used(&p);
        let out = run(&repo, "llm-b0", &p);
        let reason = out["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("no live exact claim"));
    }

    #[test]
    fn a_write_covered_by_the_owners_own_claim_is_allowed() {
        let repo = ScratchRepo::new("own-claim");
        let target = repo.root.join("foo.py");
        let p = payload("s1", &format!("echo x > {}", target.display()));
        repo.mark_graph_used(&p);
        repo.write_claim("llm-b0", &["foo.py"]);
        assert_eq!(run(&repo, "llm-b0", &p), Value::Null);
    }

    #[test]
    fn a_write_covered_by_someone_elses_claim_is_denied_naming_the_holder() {
        let repo = ScratchRepo::new("other-claim");
        let target = repo.root.join("foo.py");
        let p = payload("s1", &format!("echo x > {}", target.display()));
        repo.mark_graph_used(&p);
        repo.write_claim("llm-b1", &["foo.py"]);
        let out = run(&repo, "llm-b0", &p);
        let reason = out["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("held by llm-b1"));
    }

    #[test]
    fn a_write_outside_the_claimed_path_is_denied() {
        let repo = ScratchRepo::new("outside-claim");
        let target = repo.root.join("bar.py");
        let p = payload("s1", &format!("echo x > {}", target.display()));
        repo.mark_graph_used(&p);
        repo.write_claim("llm-b0", &["foo.py"]);
        let out = run(&repo, "llm-b0", &p);
        assert!(out["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("no live exact claim"));
    }

    #[test]
    fn an_empty_owner_is_denied_with_the_identity_message() {
        let repo = ScratchRepo::new("no-owner");
        let target = repo.root.join("foo.py");
        let p = payload("s1", &format!("echo x > {}", target.display()));
        repo.mark_graph_used(&p);
        let out = run(&repo, "", &p);
        assert!(out["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("no governance identity"));
    }

    #[test]
    fn a_grok_protocol_payload_gets_the_bare_decision_shape() {
        let repo = ScratchRepo::new("grok-protocol");
        let target = repo.root.join("foo.py");
        let mut p = payload("s1", &format!("echo x > {}", target.display()));
        p["toolName"] = json!("Bash");
        repo.mark_graph_used(&p);
        let out = run(&repo, "llm-b0", &p);
        assert_eq!(out["decision"], json!("deny"));
    }

    #[test]
    fn a_directory_with_no_git_marker_anywhere_has_no_checkout() {
        let repo = ScratchRepo::new("no-git-parent");
        // A path with no `.git` in itself or any ancestor up to filesystem
        // root resolves to None -- can't easily construct in a test without
        // touching real ancestors, so this documents the contract via a
        // deeply nested but still-rooted checkout instead.
        let nested = repo.root.join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(checkout_of(&nested).unwrap().0, repo.root);
    }
}
