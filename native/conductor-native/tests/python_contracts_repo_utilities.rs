#![cfg(feature = "python-compat-tests")]

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyList;
use support::{module, path, text, AttrPatch, Case};

#[test]
fn vault_root_derives_from_home_unless_overridden() {
    let mut case = Case::new();
    case.remove_env("CODEX_VAULT_ROOT");
    Python::attach(|py| {
        let bundle = module(py, "conductor.notebooklm_bundle");
        assert_eq!(
            text(&bundle.getattr("VAULT_ROOT_ENV").unwrap()),
            "CODEX_VAULT_ROOT"
        );
        let expected = module(py, "pathlib")
            .getattr("Path")
            .unwrap()
            .call_method0("home")
            .unwrap()
            .call_method1("joinpath", ("Documents", "CodexVault"))
            .unwrap();
        assert!(bundle
            .call_method0("_vault_root")
            .unwrap()
            .eq(expected)
            .unwrap());
        let vault = case.root().join("vault");
        case.set_env("CODEX_VAULT_ROOT", vault.to_str().unwrap());
        assert!(bundle
            .call_method0("_vault_root")
            .unwrap()
            .eq(path(py, &vault))
            .unwrap());
    });
}

fn check_paths(module_name: &str, rows: &[(&str, bool)]) {
    let _case = Case::new();
    Python::attach(|py| {
        let guard = module(py, module_name);
        for &(name, expected) in rows {
            let actual: bool = guard
                .call_method1("is_forbidden", (name,))
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(actual, expected, "{module_name} classification for {name}");
        }
    });
}

#[test]
fn rejects_each_forbidden_glob_at_the_root() {
    check_paths(
        "conductor.check_root_junk",
        &[
            ("BIG_PLAN.md", true),
            ("HANDOFF.md", true),
            ("run.log", true),
            ("metrics.jsonl", true),
            ("unused.py", true),
        ],
    );
}

#[test]
fn ignores_the_same_names_below_the_root() {
    check_paths(
        "conductor.check_root_junk",
        &[
            ("tasks/BIG_PLAN.md", false),
            ("research/reports/run.log", false),
            ("a/b/metrics.jsonl", false),
        ],
    );
}

#[test]
fn allows_the_blessed_do_not_delete_files() {
    check_paths(
        "conductor.check_root_junk",
        &[
            ("COMMANDS_DO_NOT_DELETE.txt", false),
            ("MY_PLAN_DO_NOT_DELETE.md", false),
        ],
    );
}

#[test]
fn accepts_ordinary_root_config() {
    check_paths(
        "conductor.check_root_junk",
        &[
            ("pyproject.toml", false),
            ("CLAUDE.md", false),
            ("Makefile", false),
        ],
    );
}

#[test]
fn matching_is_case_sensitive() {
    check_paths(
        "conductor.check_root_junk",
        &[("MY_PLAN.md", true), ("my_plan.md", false)],
    );
}

fn run_guard(py: Python<'_>, module_name: &str, args: &[&str]) -> i32 {
    let sys = module(py, "sys");
    let argv = PyList::new(py, std::iter::once("guard").chain(args.iter().copied())).unwrap();
    let _argv = AttrPatch::replace(sys.as_any(), "argv", argv.as_any());
    module(py, module_name)
        .call_method0("main")
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn main_exits_zero_when_nothing_is_forbidden() {
    let _case = Case::new();
    Python::attach(|py| {
        assert_eq!(
            run_guard(
                py,
                "conductor.check_root_junk",
                &["pyproject.toml", "CLAUDE.md"]
            ),
            0
        );
        assert_eq!(run_guard(py, "conductor.check_root_junk", &[]), 0);
    });
}

#[test]
fn main_exits_one_and_names_only_the_offender() {
    let _case = Case::new();
    Python::attach(|py| {
        let sys = module(py, "sys");
        let capture = module(py, "io").call_method0("StringIO").unwrap();
        let _stderr = AttrPatch::replace(sys.as_any(), "stderr", &capture);
        assert_eq!(
            run_guard(
                py,
                "conductor.check_root_junk",
                &["pyproject.toml", "PLAN.md"]
            ),
            1
        );
        let err = text(&capture.call_method0("getvalue").unwrap());
        assert!(err.contains("PLAN.md"), "{err}");
        assert!(!err.contains("pyproject.toml"), "{err}");
    });
}

#[test]
fn notes_rejects_data_at_the_top_level() {
    check_paths(
        "conductor.check_json_in_notes",
        &[
            ("research/notes/a.json", true),
            ("research/notes/b.jsonl", true),
            ("research/notes/c.csv", true),
        ],
    );
}

#[test]
fn notes_keeps_the_knowledge_tree_writable() {
    check_paths(
        "conductor.check_json_in_notes",
        &[
            ("research/notes/kb_landing_and_gate.md", false),
            ("research/notes/dead_code_audit_2026-08-27.md", false),
        ],
    );
}

#[test]
fn notes_exempts_subdirectories() {
    check_paths(
        "conductor.check_json_in_notes",
        &[
            ("research/notes/mixer_fingerprint/run.json", false),
            ("research/notes/archive/old.csv", false),
        ],
    );
}

#[test]
fn notes_ignores_data_outside_the_notes_tree() {
    check_paths(
        "conductor.check_json_in_notes",
        &[
            ("research/data/inputs.json", false),
            ("research/notes_extra/x.json", false),
            ("notes/x.json", false),
        ],
    );
}

#[test]
fn notes_main_exits_zero_when_nothing_is_forbidden() {
    let mut case = Case::new();
    case.set_env("CONDUCTOR_NOTES_ROOT", "research/notes");
    Python::attach(|py| {
        assert_eq!(
            run_guard(
                py,
                "conductor.check_json_in_notes",
                &["research/data/inputs.json", "pyproject.toml"]
            ),
            0
        );
        assert_eq!(run_guard(py, "conductor.check_json_in_notes", &[]), 0);
    });
}

#[test]
fn notes_main_exits_one_and_names_only_the_offender() {
    let mut case = Case::new();
    case.set_env("CONDUCTOR_NOTES_ROOT", "research/notes");
    Python::attach(|py| {
        let sys = module(py, "sys");
        let capture = module(py, "io").call_method0("StringIO").unwrap();
        let _stderr = AttrPatch::replace(sys.as_any(), "stderr", &capture);
        assert_eq!(
            run_guard(
                py,
                "conductor.check_json_in_notes",
                &["research/data/inputs.json", "research/notes/bulk.json"]
            ),
            1
        );
        let err = text(&capture.call_method0("getvalue").unwrap());
        assert!(err.contains("research/notes/bulk.json"), "{err}");
        assert!(!err.contains("research/data/inputs.json"), "{err}");
    });
}
