//! Pure decision seam used by the Python project-context I/O boundary.

#[path = "project_context_git.rs"]
mod git;
#[path = "project_context_identity.rs"]
mod identity;
#[path = "project_context_paths.rs"]
mod paths;
#[path = "project_context_provenance.rs"]
mod provenance;

use serde::Serialize;
use serde_json::{json, Value};

type NativeResult<T> = Result<T, NativeError>;

#[derive(Debug, Serialize)]
struct NativeError {
    code: &'static str,
    field: Option<&'static str>,
    message: String,
}

impl NativeError {
    fn new(code: &'static str, field: Option<&'static str>, message: impl Into<String>) -> Self {
        Self {
            code,
            field,
            message: message.into(),
        }
    }
}

/// Return expected refusals as data so Python can preserve `ContextError`.
pub fn evaluate(operation: &str, payload: &Value) -> Value {
    let result = match operation {
        "select-project" => paths::select_project(payload),
        "git-probe" => paths::git_probe(payload),
        "bounded-git" => git::bounded_git(payload),
        "git-topology" => paths::git_topology(payload),
        "git-membership" => paths::git_membership(payload),
        "reference-inside" => paths::reference_inside(payload),
        "config-ancestry" => paths::config_ancestry(payload),
        "safe-derived" => paths::safe_derived(payload),
        "note-authorized" => paths::note_authorized(payload),
        "keys" => identity::keys(payload),
        "derived-layout" => identity::derived_layout(payload),
        "config-read" => identity::config_read(payload),
        "assemble" => provenance::assemble(payload),
        _ => Err(NativeError::new(
            "INVALID_ARGUMENT",
            None,
            format!("unknown native project-context operation: {operation}"),
        )),
    };
    match result {
        Ok(value) => json!({"value": value}),
        Err(error) => json!({"error": error}),
    }
}

#[cfg(test)]
mod tests {
    use super::evaluate;
    use super::paths::{encode_hex, path_hex};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "forge-project-context-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn value(operation: &str, payload: Value) -> Value {
        let result = evaluate(operation, &payload);
        assert!(result.get("error").is_none(), "{result}");
        result["value"].clone()
    }

    fn code(operation: &str, payload: Value) -> String {
        evaluate(operation, &payload)["error"]["code"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn identity_keys_bind_common_git_and_top_without_writing() {
        let request = json!({"common_hex": encode_hex(b"/repository/.git"),
            "git_hex": encode_hex(b"/repository/.git/worktrees/linked"),
            "root_hex": encode_hex(b"/linked")});
        let result = value("keys", request.clone());
        let repository = Sha256::digest(b"conductor.repository.v1\0/repository/.git");
        let worktree =
            Sha256::digest(b"conductor.worktree.v1\0/repository/.git/worktrees/linked\0/linked");
        assert_eq!(result["repository_key"], format!("repo-v1-{repository:x}"));
        assert_eq!(result["worktree_key"], format!("wt-v1-{worktree:x}"));
        let changed = value(
            "keys",
            json!({"common_hex":request["common_hex"],
            "git_hex":request["git_hex"], "root_hex":encode_hex(b"/other")}),
        );
        assert_eq!(changed["repository_key"], result["repository_key"]);
        assert_ne!(changed["worktree_key"], result["worktree_key"]);
    }

    #[test]
    fn project_selection_preserves_argument_environment_default_priority() {
        let scratch = Scratch::new();
        let argument = scratch.path().join("argument");
        let environment = scratch.path().join("environment");
        fs::create_dir(&argument).unwrap();
        fs::create_dir(&environment).unwrap();
        let selected = value(
            "select-project",
            json!({"invocation_hex":path_hex(scratch.path()),
            "argument_hex":path_hex(&argument), "environment_hex":path_hex(&environment)}),
        );
        assert_eq!(selected["kind"], "argument");
        assert_eq!(selected["selected_hex"], path_hex(&argument));
        let selected = value(
            "select-project",
            json!({"invocation_hex":path_hex(scratch.path()),
            "environment_hex":path_hex(&environment)}),
        );
        assert_eq!(selected["kind"], "environment");
        let selected = value(
            "select-project",
            json!({"invocation_hex":path_hex(scratch.path())}),
        );
        assert_eq!(selected["kind"], "default");
        assert_eq!(
            code(
                "select-project",
                json!({"invocation_hex":path_hex(scratch.path()),
            "environment_hex":""})
            ),
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            code(
                "select-project",
                json!({"invocation_hex":path_hex(scratch.path()),
            "environment_hex":encode_hex(b"relative")})
            ),
            "INVALID_PATH"
        );
    }

    #[test]
    fn git_probe_distinguishes_unsupported_and_malformed_responses() {
        value("git-probe", json!({"raw_hex":encode_hex(b"true\nfalse\n")}));
        assert_eq!(
            code("git-probe", json!({"raw_hex":encode_hex(b"false\ntrue\n")})),
            "UNSUPPORTED_REPOSITORY"
        );
        assert_eq!(
            code("git-probe", json!({"raw_hex":encode_hex(b"true\n")})),
            "GIT_DISCOVERY_FAILED"
        );
    }

    #[test]
    fn git_topology_requires_three_absolute_existing_directories() {
        let scratch = Scratch::new();
        let git = scratch.path().join(".git");
        fs::create_dir(&git).unwrap();
        let raw = [
            scratch.path().as_os_str().as_encoded_bytes(),
            git.as_os_str().as_encoded_bytes(),
            git.as_os_str().as_encoded_bytes(),
        ]
        .join(&b'\n');
        let mut raw = raw;
        raw.push(b'\n');
        let parsed = value("git-topology", json!({"raw_hex":encode_hex(&raw)}));
        assert_eq!(parsed["paths_hex"][0], path_hex(scratch.path()));
        assert_eq!(parsed["paths_hex"][1], path_hex(&git));
        let bad = [
            b"relative".as_slice(),
            git.as_os_str().as_encoded_bytes(),
            git.as_os_str().as_encoded_bytes(),
        ]
        .join(&b'\n');
        assert_eq!(
            code("git-topology", json!({"raw_hex":encode_hex(&bad)})),
            "GIT_DISCOVERY_FAILED"
        );
        assert_eq!(
            code("git-topology", json!({"raw_hex":encode_hex(b"one\ntwo\n")})),
            "GIT_DISCOVERY_FAILED"
        );
    }

    #[test]
    fn path_membership_uses_components_not_prefixes() {
        let scratch = Scratch::new();
        let root = scratch.path().join("repo");
        let sibling = scratch.path().join("repo-other");
        value(
            "reference-inside",
            json!({"path_hex":path_hex(&root.join("file")),
            "root_hex":path_hex(&root), "field":"policy"}),
        );
        assert_eq!(
            code(
                "reference-inside",
                json!({"path_hex":path_hex(&sibling.join("file")),
            "root_hex":path_hex(&root), "field":"policy"})
            ),
            "PATH_OUTSIDE_PROJECT"
        );
        assert_eq!(
            code(
                "git-membership",
                json!({"selected_hex":path_hex(&sibling),
            "top_hex":path_hex(&root)})
            ),
            "PROJECT_MISMATCH"
        );
    }

    #[test]
    fn configuration_ancestry_refuses_dangling_symlink_and_non_directory() {
        let scratch = Scratch::new();
        let holder = scratch.path().join(".conductor");
        std::os::unix::fs::symlink(scratch.path().join("missing"), &holder).unwrap();
        let request = json!({"candidate_hex":path_hex(&holder.join("project.toml")),
            "root_hex":path_hex(scratch.path())});
        assert_eq!(code("config-ancestry", request.clone()), "CONFIG_IO");
        fs::remove_file(&holder).unwrap();
        fs::write(&holder, "file").unwrap();
        assert_eq!(code("config-ancestry", request), "CONFIG_IO");
    }

    #[test]
    fn derived_layout_and_existing_ancestor_refusal_are_read_only() {
        let scratch = Scratch::new();
        let layout = value(
            "derived-layout",
            json!({"common_hex":path_hex(scratch.path()),
            "repository_key":"repo-v1-abc", "worktree_key":"wt-v1-def"}),
        );
        let state = layout["state_hex"].as_str().unwrap();
        assert!(state.starts_with(&path_hex(scratch.path())));
        value(
            "safe-derived",
            json!({"common_hex":path_hex(scratch.path()),
            "path_hex":state,"field":"state_dir"}),
        );
        assert!(!scratch.path().join("conductor").exists());
        std::os::unix::fs::symlink("/tmp", scratch.path().join("conductor")).unwrap();
        assert_eq!(
            code(
                "safe-derived",
                json!({"common_hex":path_hex(scratch.path()),
            "path_hex":state,"field":"state_dir"})
            ),
            "UNSAFE_STATE_PATH"
        );
    }

    #[test]
    fn external_notes_need_an_exact_caller_grant() {
        let scratch = Scratch::new();
        let project = scratch.path().join("project");
        let granted = scratch.path().join("granted");
        let other = scratch.path().join("granted-other");
        value(
            "note-authorized",
            json!({"path_hex":path_hex(&project.join("notes")),
            "root_hex":path_hex(&project), "allowed_hex":[]}),
        );
        value(
            "note-authorized",
            json!({"path_hex":path_hex(&granted.join("notes")),
            "root_hex":path_hex(&project), "allowed_hex":[path_hex(&granted)]}),
        );
        assert_eq!(
            code(
                "note-authorized",
                json!({"path_hex":path_hex(&other),
            "root_hex":path_hex(&project), "allowed_hex":[path_hex(&granted)]})
            ),
            "PATH_OUTSIDE_PROJECT"
        );
    }

    #[test]
    fn config_read_rejects_oversize_before_changed_signature() {
        assert_eq!(
            code("config-read", json!({"size":65_537,"signatures":[1,2,3]})),
            "CONFIG_TOO_LARGE"
        );
        assert_eq!(
            code("config-read", json!({"size":12,"signatures":[1,2,1]})),
            "INPUT_CHANGED"
        );
        value("config-read", json!({"size":12,"signatures":[1,1,1]}));
    }
}
