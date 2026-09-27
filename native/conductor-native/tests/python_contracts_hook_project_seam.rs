#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for generic Claude hook and project extension seams.

#[path = "python_contracts/hook_project_seam_support.rs"]
mod seam;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule};
use serde_json::{json, Value};
use std::fs::{self, File, FileTimes};
use std::path::Path;
use std::process::Command;
use std::time::UNIX_EPOCH;
use support::{module, path, text, AttrPatch, Case};

fn py_json<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn field(value: &Bound<'_, PyAny>, key: &str) -> String {
    value.get_item(key).unwrap().extract().unwrap()
}

fn obsidian<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    let loaded = module(py, "tooling.hooks.claude.obsidian_sync");
    module(py, "importlib")
        .getattr("reload")
        .unwrap()
        .call1((loaded,))
        .unwrap()
        .cast_into::<PyModule>()
        .unwrap()
}

fn base_case() -> Case {
    let mut case = Case::new();
    case.remove_env("CLAUDE_PROJECT_DIR");
    case.remove_env("PROJECT_DIR");
    case.remove_env("WORKSPACE_NOTES_DIR");
    case.remove_env("OBSIDIAN_VAULT_ROOT");
    case.remove_env("CLAUDE_MEMORY_ROOT");
    case
}

#[test]
fn merge_appends_without_replacing_generic_context() {
    let _case = base_case();
    Python::attach(|py| {
        let append = module(py, "tooling.hooks.claude._append_context");
        let payload = py_json(
            py,
            json!({"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"generic"}}),
        );
        let out = append
            .getattr("merge")
            .unwrap()
            .call1((&payload, "project"))
            .unwrap();
        let specific = out.get_item("hookSpecificOutput").unwrap();
        assert_eq!(field(&specific, "additionalContext"), "generic\n\nproject");
        assert_eq!(field(&specific, "hookEventName"), "SessionStart");
        assert_eq!(
            field(
                &payload.get_item("hookSpecificOutput").unwrap(),
                "additionalContext"
            ),
            "generic"
        );
    });
}

#[test]
fn merge_creates_field_and_ignores_empty_or_bad_input() {
    let _case = base_case();
    Python::attach(|py| {
        let append = module(py, "tooling.hooks.claude._append_context");
        let payload = py_json(
            py,
            json!({"hookSpecificOutput":{"hookEventName":"SessionStart"}}),
        );
        let out = append
            .getattr("merge")
            .unwrap()
            .call1((payload, " p \n"))
            .unwrap();
        assert_eq!(
            field(
                &out.get_item("hookSpecificOutput").unwrap(),
                "additionalContext"
            ),
            "p"
        );
        let payload = py_json(
            py,
            json!({"hookSpecificOutput":{"additionalContext":"generic"}}),
        );
        let unchanged = append
            .getattr("merge")
            .unwrap()
            .call1((&payload, "   "))
            .unwrap();
        assert!(unchanged.is(&payload));
        let bad = append
            .getattr("merge")
            .unwrap()
            .call1(("not a dict", "p"))
            .unwrap();
        assert_eq!(text(&bad), "not a dict");
    });
}

#[test]
fn append_context_cli_passes_through_when_unset() {
    let raw = r#"{"hookSpecificOutput": {"additionalContext": "generic"}}"#;
    let script = seam::hooks().join("_append_context.py");
    let run = |input: &str, project_context: Option<&str>| {
        let mut child = Command::new(seam::python());
        child.arg(&script).env_remove("PROJECT_HOOK_CONTEXT");
        if let Some(value) = project_context {
            child.env("PROJECT_HOOK_CONTEXT", value);
        }
        use std::io::Write;
        use std::process::Stdio;
        let mut child = child
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let out = run(raw, None);
    seam::assert_success(&out);
    assert_eq!(out.stdout, raw.as_bytes());
    let out = run("not json", Some("p"));
    seam::assert_success(&out);
    assert_eq!(out.stdout, b"not json");
}

#[test]
fn session_start_runs_generic_only_when_project_is_absent() {
    let scratch = seam::Scratch::new();
    let root = scratch.host();
    assert!(!root.join(".claude/hooks/project").exists());
    let out = seam::session(&root, seam::ForgeSelection::Disabled, None, None, None);
    seam::assert_success(&out);
    assert_eq!(seam::context_text(&out), "GENERIC");
}

#[test]
fn session_start_appends_project_extension_output() {
    let scratch = seam::Scratch::new();
    let root = scratch.host();
    let marker = root.join("stdin.json");
    seam::project_hook(&root, &format!(
        "#!/usr/bin/env bash\nset -euo pipefail\ncat > {}\necho PROJECT-SIDE\necho root=$REPO_ROOT >&2\n",
        marker.display()
    ));
    let out = seam::session(&root, seam::ForgeSelection::Disabled, None, None, None);
    seam::assert_success(&out);
    assert_eq!(seam::context_text(&out), "GENERIC\n\nPROJECT-SIDE");
    let observed: Value = serde_json::from_slice(&fs::read(marker).unwrap()).unwrap();
    assert_eq!(observed, seam::payload());
    assert!(String::from_utf8_lossy(&out.stderr).contains(&format!("root={}", root.display())));
}

#[test]
fn session_start_survives_failing_project_extension() {
    let scratch = seam::Scratch::new();
    let root = scratch.host();
    seam::project_hook(&root, "#!/usr/bin/env bash\necho noise\nexit 3\n");
    let out = seam::session(&root, seam::ForgeSelection::Disabled, None, None, None);
    seam::assert_success(&out);
    assert_eq!(seam::context_text(&out), "GENERIC");
    assert!(String::from_utf8_lossy(&out.stderr).contains("project extension failed"));
}

#[test]
fn session_start_prefers_forge_when_it_resolves() {
    let scratch = seam::Scratch::new();
    let root = scratch.host();
    let marker = root.join("forge-args.txt");
    let forge = root.join("bin/forge");
    seam::link_child(&forge);
    let out = seam::session(
        &root,
        seam::ForgeSelection::Explicit(&forge),
        None,
        None,
        Some(&marker),
    );
    seam::assert_success(&out);
    assert_eq!(seam::context_text(&out), "FORGE-PREAMBLE");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("python preamble (slower)"));
    let args = fs::read_to_string(&marker).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "session",
            "preamble",
            "--host",
            root.to_str().unwrap(),
            "--a2a-name",
            ""
        ]
    );
    assert_eq!(
        fs::read_to_string(marker.with_extension("exe")).unwrap(),
        forge.to_str().unwrap()
    );

    // A selected interpreter without a sibling Forge permits PATH fallback.
    fs::remove_file(&marker).unwrap();
    let no_sibling_python = root.join("isolated/python");
    seam::link_child(&no_sibling_python);
    let out = seam::session(
        &root,
        seam::ForgeSelection::Automatic,
        Some(&no_sibling_python),
        forge.parent(),
        Some(&marker),
    );
    seam::assert_success(&out);
    assert_eq!(seam::context_text(&out), "FORGE-PREAMBLE");
    assert_eq!(
        fs::read_to_string(marker).unwrap().lines().nth(3),
        root.to_str()
    );
    assert_eq!(
        fs::read_to_string(root.join("forge-args.exe")).unwrap(),
        forge.to_str().unwrap()
    );
}

#[test]
fn session_start_prefers_selected_venv_sibling_over_path() {
    let scratch = seam::Scratch::new();
    let root = scratch.host();
    let installed = root.join(".venv/bin/forge");
    seam::link_child(&installed);
    let path_forge = root.join("path-bin/forge");
    seam::link_child(&path_forge);
    let marker = root.join("forge-args.txt");
    let out = seam::session(
        &root,
        seam::ForgeSelection::Automatic,
        None,
        path_forge.parent(),
        Some(&marker),
    );
    seam::assert_success(&out);
    assert_eq!(seam::context_text(&out), "FORGE-PREAMBLE");
    assert_eq!(
        fs::read_to_string(marker.with_extension("exe")).unwrap(),
        installed.to_str().unwrap()
    );
    fs::remove_file(path_forge).unwrap();
    let out = seam::session(
        &root,
        seam::ForgeSelection::Automatic,
        None,
        None,
        Some(&marker),
    );
    seam::assert_success(&out);
    assert_eq!(seam::context_text(&out), "FORGE-PREAMBLE");
    assert_eq!(
        fs::read_to_string(marker.with_extension("exe")).unwrap(),
        installed.to_str().unwrap()
    );
}

#[test]
fn session_start_ignores_non_executable_project_file() {
    let scratch = seam::Scratch::new();
    let root = scratch.host();
    let hook = seam::project_hook(&root, "#!/usr/bin/env bash\necho PROJECT-SIDE\n");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o644)).unwrap();
    let out = seam::session(&root, seam::ForgeSelection::Disabled, None, None, None);
    seam::assert_success(&out);
    assert_eq!(seam::context_text(&out), "GENERIC");
}

#[test]
fn project_extension_prunes_only_what_it_is_given() {
    let scratch = seam::Scratch::new();
    let stale = scratch.path().join("scratch");
    fs::create_dir_all(stale.join("old")).unwrap();
    fs::write(stale.join("old.txt"), "x").unwrap();
    fs::write(stale.join("new.txt"), "x").unwrap();
    let old = FileTimes::new().set_modified(UNIX_EPOCH);
    File::open(stale.join("old"))
        .unwrap()
        .set_times(old)
        .unwrap();
    File::open(stale.join("old.txt"))
        .unwrap()
        .set_times(old)
        .unwrap();
    let script = scratch.write("prune.sh", &format!(
        "#!/usr/bin/env bash\nset -euo pipefail\nsource {}\nprune_root_files '{}' 7 scratch\nprune_stale_subdirs '{}' 7 scratch\n",
        seam::hooks().join("_prune.sh").display(), stale.display(), stale.display()
    ));
    let out = Command::new("bash").arg(script).output().unwrap();
    seam::assert_success(&out);
    assert!(!stale.join("old.txt").exists());
    assert!(!stale.join("old").exists());
    assert!(stale.join("new.txt").exists());
}

#[test]
fn obsidian_sync_resolves_repo_root_from_own_location() {
    let case = base_case();
    Python::attach(|py| {
        let obs = obsidian(py);
        let root = text(&obs.getattr("repo_root").unwrap().call0().unwrap());
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        assert_eq!(root, source_root.to_str().unwrap());
        assert!(source_root
            .join("src/tooling/hooks/claude/obsidian_sync.py")
            .is_file());

        let stray = case
            .root()
            .join("elsewhere/tooling/hooks/claude/obsidian_sync.py");
        fs::create_dir_all(stray.parent().unwrap()).unwrap();
        fs::copy(seam::hooks().join("obsidian_sync.py"), &stray).unwrap();
        let runpy = module(py, "runpy");
        let namespace = runpy
            .getattr("run_path")
            .unwrap()
            .call1((stray.to_str().unwrap(),))
            .unwrap();
        assert_eq!(
            text(&namespace.get_item("REPO_ROOT").unwrap()),
            case.root().join("elsewhere").to_str().unwrap()
        );
    });
}

#[test]
fn obsidian_sync_honors_project_dir_and_notes_env() {
    let mut case = base_case();
    case.mkdir("elsewhere/notes");
    let root_text = case.root().to_str().unwrap().to_owned();
    case.set_env("CLAUDE_PROJECT_DIR", &root_text);
    case.set_env("WORKSPACE_NOTES_DIR", "elsewhere/notes");
    let vault = case.root().join("vault");
    let vault_text = vault.to_str().unwrap().to_owned();
    case.set_env("OBSIDIAN_VAULT_ROOT", &vault_text);
    Python::attach(|py| {
        let obs = obsidian(py);
        assert_eq!(
            text(&obs.getattr("REPO_ROOT").unwrap()),
            case.root().to_str().unwrap()
        );
        assert_eq!(
            text(&obs.getattr("NOTES_SOURCE").unwrap()),
            case.root().join("elsewhere/notes").to_str().unwrap()
        );
        assert_eq!(
            text(&obs.getattr("RESEARCH_VAULT").unwrap()),
            vault.join("research").to_str().unwrap()
        );
        assert_eq!(
            text(&obs.getattr("VAULT_ROOT").unwrap()),
            vault.join("claude").to_str().unwrap()
        );
        let memory = obs.getattr("MEMORY_ROOT").unwrap();
        assert_eq!(field_attr(&memory, "name"), "memory");
        let slug = case.root().to_str().unwrap().replace(['/', '_', '.'], "-");
        assert_eq!(field_attr(&memory.getattr("parent").unwrap(), "name"), slug);
        assert!(!field_attr(&memory.getattr("parent").unwrap(), "name").contains('/'));
    });
}

fn field_attr(value: &Bound<'_, PyAny>, name: &str) -> String {
    value.getattr(name).unwrap().extract().unwrap()
}

#[test]
fn obsidian_sync_defaults_notes_to_research_notes_under_root() {
    let mut case = base_case();
    let root_text = case.root().to_str().unwrap().to_owned();
    case.set_env("CLAUDE_PROJECT_DIR", &root_text);
    Python::attach(|py| {
        let obs = obsidian(py);
        assert_eq!(
            text(&obs.getattr("NOTES_SOURCE").unwrap()),
            case.root().join("research/notes").to_str().unwrap()
        );
    });
}

#[test]
fn obsidian_sync_honors_launcher_project_dir() {
    let mut case = base_case();
    let launched = case.root().join("launched");
    let launched_text = launched.to_str().unwrap().to_owned();
    case.set_env("PROJECT_DIR", &launched_text);
    Python::attach(|py| {
        let obs = obsidian(py);
        assert_eq!(
            text(&obs.getattr("repo_root").unwrap().call0().unwrap()),
            launched.to_str().unwrap()
        );
        let claude = case.root().join("claude");
        let claude_text = claude.to_str().unwrap().to_owned();
        case.set_env("CLAUDE_PROJECT_DIR", &claude_text);
        assert_eq!(
            text(&obs.getattr("repo_root").unwrap().call0().unwrap()),
            claude.to_str().unwrap()
        );
    });
}

#[test]
fn obsidian_sync_fails_loud_when_notes_dir_missing() {
    let case = base_case();
    Python::attach(|py| {
        let obs = obsidian(py);
        let absent = case.root().join("absent");
        let _notes = AttrPatch::replace(obs.as_any(), "NOTES_SOURCE", &path(py, &absent));
        let sys = PyModule::import(py, "sys").unwrap();
        let args = py_json(py, json!(["obsidian_sync.py", "sync-notes"]));
        let _argv = AttrPatch::replace(sys.as_any(), "argv", &args);
        let stderr = PyModule::import(py, "io")
            .unwrap()
            .getattr("StringIO")
            .unwrap()
            .call0()
            .unwrap();
        let _stderr = AttrPatch::replace(sys.as_any(), "stderr", &stderr);
        let error = obs.getattr("cmd_sync_notes").unwrap().call0().unwrap_err();
        let system_exit = PyModule::import(py, "builtins")
            .unwrap()
            .getattr("SystemExit")
            .unwrap();
        assert!(error.matches(py, &system_exit).unwrap());
        let code: i32 = error.value(py).getattr("code").unwrap().extract().unwrap();
        assert_eq!(code, 1);
        let message: String = stderr.call_method0("getvalue").unwrap().extract().unwrap();
        assert!(message.contains("WORKSPACE_NOTES_DIR"));
    });
}
