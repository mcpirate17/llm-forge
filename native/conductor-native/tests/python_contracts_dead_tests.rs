#![cfg(feature = "python-compat-tests")]
//! Dead-test audit root, report, and Python/native error contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::Command;
use support::{assert_error, module, path, text, AttrPatch, Case};

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run fixture git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(root: &Path) {
    fs::create_dir_all(root).unwrap();
    for args in [
        &["init", "-b", "main"][..],
        &["config", "user.email", "governance-tests@example.invalid"],
        &["config", "user.name", "Governance Tests"],
        &["config", "commit.gpgsign", "false"],
    ] {
        git(root, args);
    }
}

fn write(root: &Path, relative: &str, body: &str) {
    let target = root.join(relative);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, body).unwrap();
}

fn commit(root: &Path) {
    git(root, &["add", "--all"]);
    git(root, &["commit", "-m", "base"]);
}

fn broken_repo(root: &Path) {
    init_repo(root);
    write(root, "pkg/__init__.py", "");
    write(root, "test_probe.py", "import pkg.missing_module\n");
    commit(root);
}

fn empty_repo(root: &Path) {
    init_repo(root);
    git(root, &["commit", "--allow-empty", "-m", "base"]);
}

fn py_json(value: &Bound<'_, PyAny>) -> Value {
    let dumped: String = value
        .py()
        .import("json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&dumped).unwrap()
}

fn call_main(dead: &Bound<'_, PyModule>, args: Vec<String>) -> i32 {
    dead.getattr("main")
        .unwrap()
        .call1((args,))
        .unwrap()
        .extract()
        .unwrap()
}

fn output_stream<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap()
}

fn root_args(root: &Path) -> Vec<String> {
    vec!["--root".into(), root.to_str().unwrap().into()]
}

#[test]
fn explicit_root_scans_target_instead_of_cwd() {
    let case = Case::new();
    let target = case.root().join("target");
    let decoy = case.root().join("decoy");
    broken_repo(&target);
    empty_repo(&decoy);
    let _cwd = case.chdir("decoy");
    Python::attach(|py| {
        let dead = module(py, "conductor.dead_tests");
        let mut args = root_args(&target);
        args.push("--check".into());
        assert_eq!(call_main(&dead, args), 1);
    });
}

#[test]
fn default_root_uses_cwd_worktree_instead_of_module_checkout() {
    let case = Case::new();
    broken_repo(&case.root().join("repo"));
    let _cwd = case.chdir("repo");
    Python::attach(|py| {
        let dead = module(py, "conductor.dead_tests");
        assert_eq!(call_main(&dead, vec!["--check".into()]), 1);
    });
}

#[test]
fn cwd_outside_worktree_returns_audit_error() {
    let case = Case::new();
    let _cwd = case.chdir("not_a_repo");
    Python::attach(|py| {
        let dead = module(py, "conductor.dead_tests");
        assert_eq!(call_main(&dead, vec![]), 2);
    });
}

#[test]
fn resolved_root_is_printed_and_explicit_json_is_written_there() {
    let case = Case::new();
    let root = case.root().join("repo");
    empty_repo(&root);
    Python::attach(|py| {
        let dead = module(py, "conductor.dead_tests");
        let sys = module(py, "sys");
        let stdout = output_stream(py);
        let _patch = AttrPatch::replace(sys.as_any(), "stdout", &stdout);
        let mut args = root_args(&root);
        args.extend(["--json-out".into(), "out.json".into()]);
        assert_eq!(call_main(&dead, args), 0);
        assert!(text(&stdout.call_method0("getvalue").unwrap())
            .contains(&format!("root={}", root.canonicalize().unwrap().display())));
        assert!(root.join("out.json").is_file());
    });
}

#[test]
fn root_mismatch_warns_with_the_target_path() {
    let case = Case::new();
    let target = case.root().join("target");
    let decoy = case.root().join("decoy");
    empty_repo(&target);
    init_repo(&decoy);
    let _cwd = case.chdir("decoy");
    Python::attach(|py| {
        let dead = module(py, "conductor.dead_tests");
        let sys = module(py, "sys");
        let stderr = output_stream(py);
        let _patch = AttrPatch::replace(sys.as_any(), "stderr", &stderr);
        assert_eq!(call_main(&dead, root_args(&target)), 0);
        let warning = text(&stderr.call_method0("getvalue").unwrap());
        assert!(warning.contains("WARNING"), "{warning}");
        assert!(warning.contains(target.canonicalize().unwrap().to_str().unwrap()));
    });
}

#[test]
fn default_json_path_resolves_against_explicit_root() {
    let case = Case::new();
    let root = case.root().join("target");
    empty_repo(&root);
    Python::attach(|py| {
        let dead = module(py, "conductor.dead_tests");
        assert_eq!(call_main(&dead, root_args(&root)), 0);
        assert!(root.join("tasks/audit/dead_tests.json").is_file());
    });
}

#[test]
fn native_analysis_preserves_classification_precedence_and_order() {
    let case = Case::new();
    let root = case.root().join("repo");
    init_repo(&root);
    for (relative, body) in [
        ("Makefile", "run: pkg/configured.py\n"),
        ("app.py", "import pkg.live\n"),
        ("loader.py", "PLUGIN = \"dyn.py\"\n"),
        ("pkg/__init__.py", ""),
        ("pkg/broken_target.py", "VALUE = 1\n"),
        ("pkg/configured.py", "VALUE = 2\n"),
        ("pkg/dyn.py", "VALUE = 3\n"),
        ("pkg/live.py", "VALUE = 4\n"),
        ("pkg/orphan.py", "VALUE = 5\n"),
        ("pkg/stale.py", "def lazy():\n    import pkg.deleted\n"),
        (
            "test_broken.py",
            "import pkg.broken_target\nimport pkg.missing\n",
        ),
        ("test_configured.py", "import pkg.configured\n"),
        ("test_dynamic.py", "import pkg.dyn\n"),
        ("test_live.py", "import pkg.live\n"),
        ("test_orphan.py", "import pkg.orphan\n"),
        ("test_untracked_dep.py", "import pkg.untracked\n"),
    ] {
        write(&root, relative, body);
    }
    commit(&root);
    write(&root, "pkg/untracked.py", "VALUE = 6\n");
    write(
        &root,
        "test_untracked_extra.py",
        "def test_extra():\n    pass\n",
    );
    write(&root, "research/notes/targets.md", "pkg/orphan.py\n");

    Python::attach(|py| {
        let dead = module(py, "conductor.dead_tests");
        let kwargs = PyDict::new(py);
        kwargs.set_item("root", path(py, &root)).unwrap();
        let tracked = dead
            .getattr("tracked_files")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let report = dead
            .getattr("analyse")
            .unwrap()
            .call((tracked,), Some(&kwargs))
            .unwrap();
        let report = py_json(&report);
        let date = |key: &str| report[key][0]["last_commit"].clone();
        assert!(date("broken")
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        assert_eq!(
            report["broken"],
            json!([{"test":"test_broken.py",
            "missing":["pkg.missing"], "last_commit":date("broken")}])
        );
        assert_eq!(
            report["depends_on_untracked"],
            json!([{"test":"test_untracked_dep.py",
            "untracked":["pkg/untracked.py"],"last_commit":date("depends_on_untracked")}])
        );
        assert_eq!(
            report["untracked_importers"],
            json!({"pkg/untracked.py":["test_untracked_dep.py"]})
        );
        assert_eq!(
            report["orphan_target"],
            json!([{"test":"test_orphan.py",
            "targets":["pkg/orphan.py"],"notes_only":["pkg/orphan.py"],
            "last_commit":date("orphan_target")}])
        );
        assert_eq!(
            report["stale_imports"],
            json!([{"module":"pkg/stale.py",
            "missing":["pkg.deleted"],"last_commit":date("stale_imports")}])
        );
        assert_eq!(
            report["untracked_tests"],
            json!(["test_untracked_extra.py"])
        );
        for name in ["test_configured.py", "test_dynamic.py", "test_live.py"] {
            assert!(!report["orphan_target"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["test"] == name));
        }
    });
}

#[test]
fn native_parse_error_remains_typed_dead_tests_error() {
    let case = Case::new();
    case.write("repo/pkg/__init__.py", "");
    case.write("repo/pkg/broken.py", "if:\n");
    let root = case.root().join("repo");
    Python::attach(|py| {
        let dead = module(py, "conductor.dead_tests");
        let kwargs = PyDict::new(py);
        kwargs.set_item("root", path(py, &root)).unwrap();
        let resolver = dead
            .getattr("Resolver")
            .unwrap()
            .call((vec!["pkg/__init__.py", "pkg/broken.py"],), Some(&kwargs))
            .unwrap();
        let error = dead
            .getattr("scan_module")
            .unwrap()
            .call(("pkg/broken.py", resolver), Some(&kwargs))
            .expect_err("malformed source must fail");
        assert_error(
            py,
            error,
            &dead.getattr("DeadTestsError").unwrap(),
            "pkg/broken.py does not parse: invalid syntax (broken.py, line 1)",
        );
    });
}

#[test]
fn native_closure_preserves_missing_module_key_error() {
    let _case = Case::new();
    Python::attach(|py| {
        let dead = module(py, "conductor.dead_tests");
        let kwargs = PyDict::new(py);
        kwargs.set_item("path", "test_probe.py").unwrap();
        kwargs.set_item("deps", vec!["pkg/absent.py"]).unwrap();
        let entry = dead
            .getattr("Module")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let modules = PyDict::new(py);
        modules.set_item("test_probe.py", entry).unwrap();
        let error = dead
            .getattr("closure")
            .unwrap()
            .call1(("test_probe.py", modules))
            .expect_err("missing closure node must fail");
        assert_error(
            py,
            error,
            &py.get_type::<pyo3::exceptions::PyKeyError>().into_any(),
            "pkg/absent.py",
        );
    });
}
