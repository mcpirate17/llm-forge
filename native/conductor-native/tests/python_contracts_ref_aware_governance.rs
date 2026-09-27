#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for staged and reference-aware host governance.

#[path = "python_contracts/audit_fixture.rs"]
#[allow(dead_code)]
mod audit_fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use audit_fixture::{git, isolated_case, write};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyCFunction, PyDict};
use std::fs;
use std::path::Path;
use support::{assert_error, module, path, text, AttrPatch, Case};

fn governance_repo(case: &Case) {
    git(case.root(), &["init", "-q", "-b", "main"]);
    git(
        case.root(),
        &["config", "user.email", "governance-tests@example.invalid"],
    );
    git(case.root(), &["config", "user.name", "Governance Tests"]);
    git(case.root(), &["config", "commit.gpgsign", "false"]);
}

fn root_patches(py: Python<'_>, root: &Path) -> Vec<AttrPatch> {
    [
        "conductor.guardrail_audit",
        "conductor.check_protected_deletes",
        "conductor.check_duplicate_function_bodies",
    ]
    .iter()
    .map(|name| AttrPatch::replace(module(py, name).as_any(), "ROOT", path(py, root).as_any()))
    .collect()
}

fn commit_all(root: &Path, message: &str) -> String {
    git(root, &["add", "--all"]);
    git(root, &["commit", "-q", "-m", message]);
    git(root, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn oversized_function(name: &str) -> String {
    let assignments = (0..105)
        .map(|number| format!("    value = {number}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("def {name}():\n{assignments}\n    return value\n")
}

fn copyable_function(name: &str) -> String {
    format!(
        "def {name}(value):\n    total = value + 1\n    total *= 2\n    total -= 3\n    total //= 4\n    total += 5\n    total *= 6\n    return total\n"
    )
}

fn audit_kinds(py: Python<'_>, root: &Path, from_ref: Option<&str>, staged: bool) {
    let audit = module(py, "conductor.guardrail_audit");
    let _roots = root_patches(py, root);
    let kwargs = PyDict::new(py);
    if staged {
        kwargs.set_item("staged_only", true).unwrap();
    }
    if let Some(base) = from_ref {
        kwargs.set_item("from_ref", base).unwrap();
    }
    let result = audit
        .getattr("collect_issues")
        .unwrap()
        .call((("research",),), Some(&kwargs))
        .unwrap();
    let issues = result.get_item(0).unwrap();
    let summary = result.get_item(1).unwrap();
    assert_eq!(
        summary
            .get_item("files_scanned")
            .unwrap()
            .extract::<i32>()
            .unwrap(),
        1
    );
    let kinds = issues
        .try_iter()
        .unwrap()
        .map(|issue| text(&issue.unwrap().getattr("kind").unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(kinds, vec!["god_function"]);
}

#[test]
fn guardrail_audit_reads_staged_snapshot() {
    let case = isolated_case();
    governance_repo(&case);
    let _cwd = case.chdir(".");
    write(case.root(), "research/candidate.py", "value = 1\n");
    commit_all(case.root(), "base");
    write(
        case.root(),
        "research/candidate.py",
        &oversized_function("staged_candidate"),
    );
    git(case.root(), &["add", "research/candidate.py"]);
    write(case.root(), "research/candidate.py", "value = 2\n");
    Python::attach(|py| audit_kinds(py, case.root(), None, true));
}

#[test]
fn guardrail_audit_reads_head_for_from_ref_with_clean_index() {
    let case = isolated_case();
    governance_repo(&case);
    let _cwd = case.chdir(".");
    write(case.root(), "research/candidate.py", "value = 1\n");
    let base = commit_all(case.root(), "base");
    write(
        case.root(),
        "research/candidate.py",
        &oversized_function("committed_candidate"),
    );
    commit_all(case.root(), "candidate");
    write(case.root(), "research/candidate.py", "value = 2\n");
    assert!(git(case.root(), &["diff", "--cached", "--name-only"])
        .trim()
        .is_empty());
    Python::attach(|py| audit_kinds(py, case.root(), Some(&base), false));
}

fn protected_delete(from_ref: bool) {
    let case = isolated_case();
    governance_repo(&case);
    let _cwd = case.chdir(".");
    let protected = "research/runtime/champion_example.json";
    write(case.root(), protected, "{}\n");
    let base = commit_all(case.root(), "base");
    fs::remove_file(case.root().join(protected)).unwrap();
    if from_ref {
        commit_all(case.root(), "delete protected file");
        assert!(git(case.root(), &["diff", "--cached", "--name-only"])
            .trim()
            .is_empty());
    } else {
        git(case.root(), &["add", "--update"]);
    }
    Python::attach(|py| {
        let _roots = root_patches(py, case.root());
        let checker = module(py, "conductor.check_protected_deletes");
        let deleted = if from_ref {
            checker
                .getattr("_deleted_paths")
                .unwrap()
                .call1((base.as_str(),))
                .unwrap()
        } else {
            checker.getattr("_deleted_paths").unwrap().call0().unwrap()
        };
        assert_eq!(deleted.extract::<Vec<String>>().unwrap(), vec![protected]);
        let argv = if from_ref {
            vec!["--from-ref", base.as_str()]
        } else {
            vec![]
        };
        assert_eq!(
            checker
                .getattr("main")
                .unwrap()
                .call1((argv,))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
    });
}

#[test]
fn protected_delete_reads_staged_index() {
    protected_delete(false);
}

#[test]
fn protected_delete_reads_from_ref_with_clean_index() {
    protected_delete(true);
}

fn duplicate_pairs(py: Python<'_>, base: Option<&str>) -> Vec<(String, String)> {
    let checker = module(py, "conductor.check_duplicate_function_bodies");
    let result = if let Some(base) = base {
        checker
            .getattr("_duplicate_pairs")
            .unwrap()
            .call1((base,))
            .unwrap()
    } else {
        checker
            .getattr("_duplicate_pairs")
            .unwrap()
            .call0()
            .unwrap()
    };
    result
        .try_iter()
        .unwrap()
        .map(|pair| {
            let pair = pair.unwrap();
            (
                text(&pair.get_item(0).unwrap().getattr("path").unwrap()),
                text(&pair.get_item(1).unwrap().getattr("path").unwrap()),
            )
        })
        .collect()
}

#[test]
fn duplicate_body_reads_staged_index() {
    let case = isolated_case();
    governance_repo(&case);
    let _cwd = case.chdir(".");
    write(
        case.root(),
        "research/original.py",
        &copyable_function("original"),
    );
    commit_all(case.root(), "base");
    write(
        case.root(),
        "research/copied.py",
        &copyable_function("copied"),
    );
    git(case.root(), &["add", "research/copied.py"]);
    write(case.root(), "research/copied.py", "value = 1\n");
    Python::attach(|py| {
        let _roots = root_patches(py, case.root());
        assert_eq!(
            duplicate_pairs(py, None),
            vec![("research/copied.py".into(), "research/original.py".into())]
        );
    });
}

#[test]
fn duplicate_body_reads_from_ref_with_clean_index() {
    let case = isolated_case();
    governance_repo(&case);
    let _cwd = case.chdir(".");
    write(
        case.root(),
        "research/original.py",
        &copyable_function("original"),
    );
    let base = commit_all(case.root(), "base");
    write(
        case.root(),
        "research/copied.py",
        &copyable_function("copied"),
    );
    commit_all(case.root(), "copy body");
    assert!(git(case.root(), &["diff", "--cached", "--name-only"])
        .trim()
        .is_empty());
    Python::attach(|py| {
        let _roots = root_patches(py, case.root());
        assert_eq!(
            duplicate_pairs(py, Some(&base)),
            vec![("research/copied.py".into(), "research/original.py".into())]
        );
    });
}

#[test]
fn duplicate_body_from_ref_allows_move() {
    let case = isolated_case();
    governance_repo(&case);
    let _cwd = case.chdir(".");
    let source = "research/original.py";
    write(case.root(), source, &copyable_function("original"));
    let base = commit_all(case.root(), "base");
    fs::remove_file(case.root().join(source)).unwrap();
    write(
        case.root(),
        "research/package/moved.py",
        &copyable_function("moved"),
    );
    commit_all(case.root(), "move body");
    Python::attach(|py| {
        let _roots = root_patches(py, case.root());
        assert!(duplicate_pairs(py, Some(&base)).is_empty());
    });
}

#[test]
fn duplicate_body_git_and_cli_fail_closed() {
    let _case = isolated_case();
    Python::attach(|py| {
        let checker = module(py, "conductor.check_duplicate_function_bodies");
        let process = module(py, "subprocess")
            .getattr("CompletedProcess")
            .unwrap()
            .call1((
                Vec::<String>::new(),
                2,
                PyBytes::new(py, b"").as_any(),
                PyBytes::new(py, b"git failed").as_any(),
            ))
            .unwrap()
            .unbind();
        let failure =
            PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
                Ok(process.clone_ref(args.py()))
            })
            .unwrap();
        let _git_patch = AttrPatch::replace(checker.as_any(), "_git", failure.as_any());
        let runtime = module(py, "builtins").getattr("RuntimeError").unwrap();
        let missing_tree = checker
            .getattr("_tracked_python_files")
            .unwrap()
            .call1(("HEAD",))
            .unwrap_err();
        assert_error(py, missing_tree, &runtime, "ls-tree failed");
        let missing_diff = checker
            .getattr("_changed_python_files")
            .unwrap()
            .call0()
            .unwrap_err();
        assert_error(py, missing_diff, &runtime, "git diff failed");
        let missing_base = checker
            .getattr("_merge_base")
            .unwrap()
            .call1(("HEAD^",))
            .unwrap_err();
        assert_error(py, missing_base, &runtime, "merge base");
        let empty = PyCFunction::new_closure(py, None, None, |_, _| -> PyResult<Vec<String>> {
            Ok(vec![])
        })
        .unwrap();
        let _changed =
            AttrPatch::replace(checker.as_any(), "_changed_python_files", empty.as_any());
        let no_pairs = PyCFunction::new_closure(py, None, None, |_, _| -> PyResult<Vec<String>> {
            Ok(vec![])
        })
        .unwrap();
        let _pairs = AttrPatch::replace(checker.as_any(), "_duplicate_pairs", no_pairs.as_any());
        assert_eq!(
            checker
                .getattr("main")
                .unwrap()
                .call1((Vec::<String>::new(),))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        let class = checker.getattr("FunctionBody").unwrap();
        let new = class.call1(("new.py", "new", 1, "a")).unwrap().unbind();
        let old = class.call1(("old.py", "old", 2, "a")).unwrap().unbind();
        let pairs =
            PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
                let py = args.py();
                let items = pyo3::types::PyList::new(
                    py,
                    [pyo3::types::PyTuple::new(py, [new.bind(py), old.bind(py)])?.into_any()],
                )?;
                Ok(items.into_any().unbind())
            })
            .unwrap();
        let _some = AttrPatch::replace(checker.as_any(), "_duplicate_pairs", pairs.as_any());
        assert_eq!(
            checker
                .getattr("main")
                .unwrap()
                .call1((Vec::<String>::new(),))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
    });
}
