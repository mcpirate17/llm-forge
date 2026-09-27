#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for base-relative value-gate test selection.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use support::{module, path, Case};

const TEST_PATH: &str = "conductor/test_probe_scope.py";
const BASE: &str = "def test_kept():\n    assert True\n\n\ndef test_edited():\n    assert 1 == 1\n";

fn isolated_case() -> Case {
    let mut case = Case::new();
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "GIT_CONFIG",
        "GIT_TEMPLATE_DIR",
    ] {
        case.remove_env(name);
    }
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    case.set_env("GIT_CONFIG_SYSTEM", "/dev/null");
    case
}

fn git(repo: &Path, args: &[&str], input: Option<&[u8]>) -> String {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_TEMPLATE_DIR")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn init_repo(case: &Case) {
    let repo = case.root().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"], None);
}

fn blob(case: &Case, source: &str) -> String {
    git(
        &case.root().join("repo"),
        &["hash-object", "-w", "--stdin"],
        Some(source.as_bytes()),
    )
}

fn context<'py>(
    py: Python<'py>,
    case: &Case,
    source: &str,
    base_oid: &str,
    test_path: &str,
    old_path: Option<&str>,
) -> Bound<'py, PyAny> {
    let repo = case.root().join("repo");
    let snapshot = case.root().join("snapshot");
    let target = snapshot.join(test_path);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, source).unwrap();
    let model = module(py, "conductor.candidate_review.model");
    let change_kw = PyDict::new(py);
    change_kw.set_item("status", "M").unwrap();
    change_kw.set_item("path", test_path).unwrap();
    change_kw.set_item("old_path", old_path).unwrap();
    change_kw
        .set_item(
            "old_mode",
            if base_oid == "0".repeat(40) {
                "000000"
            } else {
                "100644"
            },
        )
        .unwrap();
    change_kw.set_item("new_mode", "100644").unwrap();
    change_kw.set_item("old_oid", base_oid).unwrap();
    change_kw.set_item("new_oid", "d".repeat(40)).unwrap();
    change_kw.set_item("classes", ("test",)).unwrap();
    let change = model
        .getattr("Change")
        .unwrap()
        .call((), Some(&change_kw))
        .unwrap();
    let candidate_kw = PyDict::new(py);
    candidate_kw.set_item("kind", "index").unwrap();
    candidate_kw.set_item("tree_oid", "a".repeat(40)).unwrap();
    candidate_kw
        .set_item("base_tree_oid", "b".repeat(40))
        .unwrap();
    candidate_kw
        .set_item("base_commit_oid", "c".repeat(40))
        .unwrap();
    candidate_kw.set_item("commit_oid", py.None()).unwrap();
    candidate_kw.set_item("target_ref", "HEAD").unwrap();
    candidate_kw.set_item("changes", (change,)).unwrap();
    let candidate = model
        .getattr("Candidate")
        .unwrap()
        .call((), Some(&candidate_kw))
        .unwrap();
    let policy_path = module(py, "conductor.candidate_review.policy_path")
        .getattr("resolve_policy_path")
        .unwrap()
        .call0()
        .unwrap();
    let policy = module(py, "conductor.candidate_review.policy")
        .getattr("load_policy")
        .unwrap()
        .call1((policy_path,))
        .unwrap();
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo", path(py, &repo)).unwrap();
    kwargs.set_item("snapshot", path(py, &snapshot)).unwrap();
    kwargs.set_item("candidate", candidate).unwrap();
    kwargs.set_item("entries", ()).unwrap();
    kwargs.set_item("policy", policy).unwrap();
    kwargs.set_item("surface", "manual").unwrap();
    kwargs.set_item("profile", "fast").unwrap();
    kwargs.set_item("owner", py.None()).unwrap();
    kwargs
        .set_item("runtime_dir", path(py, &case.root().join("runtime")))
        .unwrap();
    module(py, "conductor.candidate_review.checks")
        .getattr("ReviewContext")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn gated(
    case: &Case,
    source: &str,
    base_oid: &str,
    test_path: &str,
    old_path: Option<&str>,
    grandfathered: &[&str],
) -> PyResult<Value> {
    Python::attach(|py| {
        let ctx = context(py, case, source, base_oid, test_path, old_path);
        let exemptions = PyDict::new(py);
        if !grandfathered.is_empty() {
            let frozen = module(py, "builtins")
                .getattr("frozenset")?
                .call1((grandfathered,))?;
            exemptions.set_item(test_path, frozen)?;
        }
        let result = module(py, "conductor.candidate_review.verification")
            .getattr("_value_gated_nodeids")?
            .call1((ctx, exemptions))?;
        let encoded: String = module(py, "json")
            .getattr("dumps")?
            .call1((result,))?
            .extract()?;
        Ok(serde_json::from_str(&encoded).unwrap())
    })
}

fn expected(names: &[&str]) -> Value {
    let mut result = serde_json::Map::new();
    result.insert(
        TEST_PATH.to_owned(),
        json!(names
            .iter()
            .map(|name| format!("{TEST_PATH}::{name}"))
            .collect::<Vec<_>>()),
    );
    Value::Object(result)
}

#[test]
fn an_added_file_gates_every_definition() {
    let case = isolated_case();
    init_repo(&case);
    assert_eq!(
        gated(&case, BASE, &"0".repeat(40), TEST_PATH, None, &[]).unwrap(),
        expected(&["test_edited", "test_kept"])
    );
}

#[test]
fn a_definition_added_to_an_existing_file_is_gated() {
    let case = isolated_case();
    init_repo(&case);
    let candidate = format!("{BASE}\n\ndef test_new():\n    assert True\n");
    assert_eq!(
        gated(&case, &candidate, &blob(&case, BASE), TEST_PATH, None, &[]).unwrap(),
        expected(&["test_new"])
    );
}

#[test]
fn a_modified_definition_is_gated() {
    let case = isolated_case();
    init_repo(&case);
    let candidate = BASE.replace("assert 1 == 1", "assert 2 == 2");
    assert_eq!(
        gated(&case, &candidate, &blob(&case, BASE), TEST_PATH, None, &[]).unwrap(),
        expected(&["test_edited"])
    );
}

#[test]
fn an_untouched_definition_sharing_the_file_is_not_gated() {
    let case = isolated_case();
    init_repo(&case);
    let candidate = BASE.replace("assert 1 == 1", "assert 2 == 2");
    let result = gated(&case, &candidate, &blob(&case, BASE), TEST_PATH, None, &[]).unwrap();
    assert!(!result[TEST_PATH]
        .as_array()
        .unwrap()
        .contains(&json!(format!("{TEST_PATH}::test_kept"))));
}

#[test]
fn a_file_changed_only_outside_its_tests_gates_nothing() {
    let case = isolated_case();
    init_repo(&case);
    let candidate = format!("import os  # noqa: F401\n\n\n{BASE}");
    assert_eq!(
        gated(&case, &candidate, &blob(&case, BASE), TEST_PATH, None, &[]).unwrap(),
        json!({})
    );
}

#[test]
fn a_decorator_only_change_still_counts_as_modified() {
    let case = isolated_case();
    init_repo(&case);
    let base =
        "import pytest\n\n\n@pytest.mark.parametrize('n', [1])\ndef test_p(n):\n    assert n\n";
    let candidate = base.replace("[1]", "[1, 2]");
    assert_eq!(
        gated(&case, &candidate, &blob(&case, base), TEST_PATH, None, &[]).unwrap(),
        expected(&["test_p"])
    );
}

#[test]
fn a_grandfathered_definition_stays_excluded_when_modified() {
    let case = isolated_case();
    init_repo(&case);
    let candidate = BASE.replace("assert 1 == 1", "assert 2 == 2");
    assert_eq!(
        gated(
            &case,
            &candidate,
            &blob(&case, BASE),
            TEST_PATH,
            None,
            &["test_edited"]
        )
        .unwrap(),
        json!({})
    );
}

#[test]
fn an_unreadable_base_blob_gates_the_whole_file() {
    let case = isolated_case();
    init_repo(&case);
    assert_eq!(
        gated(&case, BASE, &"e".repeat(40), TEST_PATH, None, &[]).unwrap(),
        expected(&["test_edited", "test_kept"])
    );
}

#[test]
fn a_base_that_does_not_parse_gates_the_whole_file() {
    let case = isolated_case();
    init_repo(&case);
    assert_eq!(
        gated(
            &case,
            BASE,
            &blob(&case, "def test_kept(:\n"),
            TEST_PATH,
            None,
            &[]
        )
        .unwrap(),
        expected(&["test_edited", "test_kept"])
    );
}

#[test]
fn a_renamed_file_is_compared_against_its_old_path() {
    let case = isolated_case();
    init_repo(&case);
    let candidate = format!("{BASE}\n\ndef test_new():\n    assert True\n");
    assert_eq!(
        gated(
            &case,
            &candidate,
            &blob(&case, BASE),
            TEST_PATH,
            Some("conductor/test_probe_scope_old.py"),
            &[]
        )
        .unwrap(),
        expected(&["test_new"])
    );
}

#[test]
fn class_nested_tests_are_scoped_by_their_qualified_label() {
    let case = isolated_case();
    init_repo(&case);
    let base = "class TestThing:\n    def test_kept(self):\n        assert True\n\n    def test_edited(self):\n        assert 1 == 1\n";
    let candidate = base.replace("assert 1 == 1", "assert 2 == 2");
    assert_eq!(
        gated(&case, &candidate, &blob(&case, base), TEST_PATH, None, &[]).unwrap(),
        expected(&["TestThing::test_edited"])
    );
}

#[test]
fn a_candidate_that_does_not_parse_is_refused_not_skipped() {
    let case = isolated_case();
    init_repo(&case);
    let error = gated(
        &case,
        "def test_kept(:\n",
        &blob(&case, BASE),
        TEST_PATH,
        None,
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("cannot parse test definitions"));
}

#[test]
fn a_mutant_patch_fragment_is_never_a_test_definition() {
    let case = isolated_case();
    init_repo(&case);
    assert_eq!(
        gated(
            &case,
            "--- a/x\n+++ b/x\n",
            &"0".repeat(40),
            "conductor/mutation_campaigns/patches/c/test_thing_dropped.patch",
            None,
            &[]
        )
        .unwrap(),
        json!({})
    );
}

#[test]
fn a_new_non_python_test_file_is_gated_as_a_whole_path() {
    let case = isolated_case();
    init_repo(&case);
    let test_path = "conductor/ui/test_thing.js";
    assert_eq!(
        gated(
            &case,
            "it('works', () => {});\n",
            &"0".repeat(40),
            test_path,
            None,
            &[]
        )
        .unwrap(),
        json!({"conductor/ui/test_thing.js": ["conductor/ui/test_thing.js"]})
    );
}
