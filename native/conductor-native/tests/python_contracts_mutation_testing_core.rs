#![cfg(feature = "python-compat-tests")]
//! Campaign fixture loading, declared test scope, and host-read contracts.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_testing_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use fixture::{equal, fixture_campaign, fixture_payload, py_expected, testing};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyTuple};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, AttrPatch, Case};

fn campaign_error(py: Python<'_>, result: PyResult<Bound<'_, PyAny>>, message: &str) {
    assert_error(
        py,
        result.unwrap_err(),
        &testing(py).getattr("CampaignError").unwrap(),
        message,
    );
}

fn campaign_error_pattern(py: Python<'_>, result: PyResult<Bound<'_, PyAny>>, pattern: &str) {
    let error = result.unwrap_err();
    assert!(
        error
            .matches(py, &testing(py).getattr("CampaignError").unwrap())
            .unwrap(),
        "{error}"
    );
    let found = py
        .import("re")
        .unwrap()
        .getattr("search")
        .unwrap()
        .call1((pattern, error.to_string()))
        .unwrap();
    assert!(!found.is_none(), "expected {pattern:?} in {error}");
}

fn load<'py>(py: Python<'py>, manifest: &Path, root: &Path) -> PyResult<Bound<'py, PyAny>> {
    let kw = PyDict::new(py);
    kw.set_item("repo_root", path(py, root)).unwrap();
    testing(py)
        .getattr("load_campaign")
        .unwrap()
        .call((path(py, manifest),), Some(&kw))
}

fn ranked<'py>(
    py: Python<'py>,
    rank: usize,
    nodeid: &str,
    contract: &str,
    rationale: &str,
) -> Bound<'py, PyAny> {
    testing(py)
        .getattr("RankedTest")
        .unwrap()
        .call1((rank, nodeid, contract, rationale))
        .unwrap()
}

fn sha<'py>(py: Python<'py>, file: &Path) -> Bound<'py, PyAny> {
    testing(py)
        .getattr("_sha256")
        .unwrap()
        .call1((path(py, file),))
        .unwrap()
}

fn load_scopes<'py>(
    py: Python<'py>,
    root: &Path,
    scopes: &Bound<'py, PyAny>,
    pins: &Bound<'py, PyAny>,
    ranked_tests: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let kw = PyDict::new(py);
    kw.set_item("source_sha256", pins).unwrap();
    kw.set_item("ranked_tests", ranked_tests).unwrap();
    kw.set_item("repo_root", path(py, root)).unwrap();
    testing(py)
        .getattr("_load_test_scopes")
        .unwrap()
        .call((scopes,), Some(&kw))
}

fn test_file(case: &Case, relative: &str, contents: &str) -> PathBuf {
    case.write(relative, contents)
}

fn empty_positional_list(py: Python<'_>) -> Bound<'_, PyCFunction> {
    PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        if kwargs.is_some_and(|values| !values.is_empty()) {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "unexpected keyword argument",
            ));
        }
        Ok::<_, PyErr>(PyList::empty(args.py()).into_any().unbind())
    })
    .unwrap()
}

#[test]
fn test_campaign_ranks_every_test_contiguously_and_materializes_its_mutation() {
    let _case = Case::new();
    Python::attach(|py| {
        let campaign = fixture_campaign(py);
        let ranked = campaign.getattr("ranked_tests").unwrap();
        assert_eq!(ranked.len().unwrap(), 2);
        let ranks: Vec<i64> = ranked
            .try_iter()
            .unwrap()
            .map(|row| row.unwrap().getattr("rank").unwrap().extract().unwrap())
            .collect();
        assert_eq!(ranks, vec![1, 2]);
        assert_eq!(
            campaign
                .getattr("expected_mutations")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            1
        );
        assert_eq!(
            campaign
                .getattr("planned_mutations")
                .unwrap()
                .len()
                .unwrap(),
            1
        );
        let ids: Vec<String> = campaign
            .getattr("mutations")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|row| {
                row.unwrap()
                    .getattr("mutation_id")
                    .unwrap()
                    .extract()
                    .unwrap()
            })
            .collect();
        assert_eq!(ids, vec!["pack_mode_first_order"]);
    });
}

#[test]
fn test_inspection_reports_ready_with_its_materialized_patch() {
    let _case = Case::new();
    Python::attach(|py| {
        let campaign = fixture_campaign(py);
        let subject = testing(py);
        let source = empty_positional_list(py);
        let _source_patch = AttrPatch::replace(&subject, "source_drift", source.as_any());
        let blockers = empty_positional_list(py);
        let _blockers_patch = AttrPatch::replace(&subject, "blocking_processes", blockers.as_any());
        let result = subject
            .getattr("inspect_campaign")
            .unwrap()
            .call1((campaign,))
            .unwrap();
        assert_eq!(
            result
                .get_item("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "READY"
        );
        assert_eq!(
            result
                .get_item("materialized_mutations")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            1
        );
        assert_eq!(
            result
                .get_item("expected_mutations")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            1
        );
        assert_eq!(
            result
                .get_item("resource_status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "IDLE"
        );
        let plans = result.get_item("planned_mutations").unwrap();
        for row in plans.try_iter().unwrap() {
            assert!(row
                .unwrap()
                .get_item("materialized")
                .unwrap()
                .extract::<bool>()
                .unwrap());
        }
    });
}

#[test]
fn test_rank_gaps_fail_closed() {
    let case = Case::new();
    Python::attach(|py| {
        let mut payload = fixture_payload(py);
        payload["mutations"] = json!([]);
        assert!(payload["ranked_tests"].is_array());
        assert!(payload["ranked_tests"][1].is_object());
        payload["ranked_tests"][1]["rank"] = json!(7);
        let file = case.write("campaign.json", &payload.to_string());
        campaign_error(py, load(py, &file, case.root()), "contiguous ranks");
    });
}

#[test]
fn test_baseline_must_execute_every_ranked_test() {
    let case = Case::new();
    Python::attach(|py| {
        let mut payload = fixture_payload(py);
        payload["mutations"] = json!([]);
        assert!(payload["baseline"].is_object());
        assert!(payload["baseline"]["argv"].is_array());
        assert!(payload["ranked_tests"].is_array());
        assert!(payload["ranked_tests"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .is_object());
        let missing = payload["ranked_tests"].as_array().unwrap().last().unwrap()["nodeid"].clone();
        let argv = payload["baseline"]["argv"].as_array_mut().unwrap();
        argv.remove(argv.iter().position(|item| *item == missing).unwrap());
        let file = case.write("campaign.json", &payload.to_string());
        campaign_error(py, load(py, &file, case.root()), "omits ranked tests");
    });
}

#[test]
fn test_complete_python_scope_rejects_omitted_test_node() {
    let case = Case::new();
    Python::attach(|py| {
        let relative = "pkg/test_contract.py";
        let file = test_file(
            &case,
            relative,
            "def test_first():\n    assert True\n\ndef test_second():\n    assert True\n",
        );
        let first = format!("{relative}::test_first");
        let scopes = py_json(
            py,
            json!({relative:{"mode":"complete","inventory":"python_ast","nodeids":[first]}}),
        );
        let pins = PyDict::new(py);
        pins.set_item(relative, sha(py, &file)).unwrap();
        let ranked = PyTuple::new(py, [ranked(py, 1, &first, "first", "first")]).unwrap();
        campaign_error_pattern(
            py,
            load_scopes(py, case.root(), &scopes, pins.as_any(), ranked.as_any()),
            "missing=.*test_second",
        );
    });
}

#[test]
fn test_complete_python_scope_allows_ranked_subset_of_full_inventory() {
    let case = Case::new();
    Python::attach(|py| {
        let relative = "pkg/test_contract.py";
        let file = test_file(
            &case,
            relative,
            "def test_ranked():\n    assert True\n\ndef test_inventory_only():\n    assert True\n",
        );
        let nodeid = format!("{relative}::test_ranked");
        let inventory = [nodeid.clone(), format!("{relative}::test_inventory_only")];
        let scopes = py_json(
            py,
            json!({relative:{"mode":"complete","inventory":"python_ast","nodeids":inventory}}),
        );
        let pins = PyDict::new(py);
        pins.set_item(relative, sha(py, &file)).unwrap();
        let ranked = PyTuple::new(
            py,
            [ranked(
                py,
                1,
                &nodeid,
                "ranked attribution",
                "ranked attribution",
            )],
        )
        .unwrap();
        let result = load_scopes(py, case.root(), &scopes, pins.as_any(), ranked.as_any()).unwrap();
        equal(
            &result
                .get_item(relative)
                .unwrap()
                .getattr("nodeids")
                .unwrap(),
            &PyTuple::new(py, inventory).unwrap(),
        );
    });
}

#[test]
fn test_test_scope_rejects_ranked_node_missing_from_declared_scope() {
    let case = Case::new();
    Python::attach(|py| {
        let relative = "pkg/test_contract.py";
        let file = test_file(&case, relative, "def test_declared():\n    assert True\n");
        let missing = format!("{relative}::test_omitted_ranked");
        let scopes = py_json(
            py,
            json!({relative:{"mode":"complete","inventory":"python_ast","nodeids":[format!("{relative}::test_declared")]}}),
        );
        let pins = PyDict::new(py);
        pins.set_item(relative, sha(py, &file)).unwrap();
        let ranked = PyTuple::new(
            py,
            [ranked(
                py,
                1,
                &missing,
                "ranked membership",
                "ranked membership",
            )],
        )
        .unwrap();
        campaign_error_pattern(
            py,
            load_scopes(py, case.root(), &scopes, pins.as_any(), ranked.as_any()),
            "ranked_tests nodeids are missing.*test_omitted_ranked",
        );
    });
}

#[test]
fn test_complete_python_scope_rejects_file_without_ranked_test() {
    let case = Case::new();
    Python::attach(|py| {
        let ranked_relative = "pkg/test_ranked.py";
        let other_relative = "pkg/test_unranked.py";
        let ranked_path = test_file(
            &case,
            ranked_relative,
            "def test_ranked():\n    assert True\n",
        );
        let other_path = test_file(
            &case,
            other_relative,
            "def test_unranked():\n    assert True\n",
        );
        let nodeid = format!("{ranked_relative}::test_ranked");
        let scopes = py_json(
            py,
            json!({ranked_relative:{"mode":"complete","inventory":"python_ast","nodeids":[nodeid]},other_relative:{"mode":"complete","inventory":"python_ast","nodeids":[format!("{other_relative}::test_unranked")]}}),
        );
        let pins = PyDict::new(py);
        pins.set_item(ranked_relative, sha(py, &ranked_path))
            .unwrap();
        pins.set_item(other_relative, sha(py, &other_path)).unwrap();
        let ranked = PyTuple::new(
            py,
            [ranked(
                py,
                1,
                &nodeid,
                "ranked attribution",
                "ranked attribution",
            )],
        )
        .unwrap();
        campaign_error_pattern(
            py,
            load_scopes(py, case.root(), &scopes, pins.as_any(), ranked.as_any()),
            "complete test_scopes.*test_unranked.py.*at least one ranked test",
        );
    });
}

#[test]
fn test_blocking_processes_returns_matching_process_evidence() {
    let _case = Case::new();
    Python::attach(|py| {
        let output = "\n       11 /usr/bin/python unrelated.py\n       42 .venv/bin/python -m research.tools.nm_f6_phase22_20m_active train\n    ";
        let kw = PyDict::new(py);
        kw.set_item("process_output", output).unwrap();
        let result = testing(py)
            .getattr("blocking_processes")
            .unwrap()
            .call(
                (PyList::new(py, ["research.tools.nm_f6_phase22_20m_active train"]).unwrap(),),
                Some(&kw),
            )
            .unwrap();
        equal(
            &result,
            &py_expected(
                py,
                json!([{"pid":42,"command":".venv/bin/python -m research.tools.nm_f6_phase22_20m_active train","matched":["research.tools.nm_f6_phase22_20m_active train"]}]),
            ),
        );
    });
}

#[test]
fn test_patch_parser_rejects_file_creation_or_deletion() {
    let case = Case::new();
    Python::attach(|py| {
        let patch = case.write(
            "delete.patch",
            "diff --git a/example.py b/example.py\n--- a/example.py\n+++ /dev/null\n",
        );
        campaign_error(
            py,
            testing(py)
                .getattr("_patch_paths")
                .unwrap()
                .call1((path(py, &patch),)),
            "create or delete",
        );
    });
}

#[test]
fn test_host_read_dependencies_are_materialized_not_symlinked() {
    let case = Case::new();
    Python::attach(|py| {
        case.write("host/reports/screen/receipt.json", "{}");
        case.write("host/notes/plan.md", "# plan\n");
        let snapshot = case.mkdir("snapshot");
        let dataclasses = module(py, "dataclasses");
        let kwargs = PyDict::new(py);
        kwargs
            .set_item(
                "host_read_dependencies",
                PyTuple::new(py, ["reports/screen", "notes/plan.md"]).unwrap(),
            )
            .unwrap();
        let campaign = dataclasses
            .getattr("replace")
            .unwrap()
            .call((fixture_campaign(py),), Some(&kwargs))
            .unwrap();
        testing(py)
            .getattr("_link_host_dependencies")
            .unwrap()
            .call1((
                campaign,
                path(py, &snapshot),
                path(py, &case.root().join("host")),
            ))
            .unwrap();
        let nested = snapshot.join("reports/screen/receipt.json");
        let flat = snapshot.join("notes/plan.md");
        for linked in [&nested, &flat, &snapshot.join("reports/screen")] {
            assert!(linked.exists());
            assert!(!linked.is_symlink());
        }
        assert!(nested
            .canonicalize()
            .unwrap()
            .starts_with(snapshot.canonicalize().unwrap()));
        assert_eq!(fs::read_to_string(nested).unwrap(), "{}");
        assert_eq!(fs::read_to_string(flat).unwrap(), "# plan\n");
    });
}

#[test]
fn test_manifest_rejects_patch_path_escape() {
    let case = Case::new();
    Python::attach(|py| {
        let mut payload = fixture_payload(py);
        assert!(payload["mutations"].is_array());
        assert!(payload["mutations"][0].is_object());
        payload["mutations"][0]["patch_file"] = json!("../../outside.patch");
        let file = case.write("campaign.json", &payload.to_string());
        campaign_error(py, load(py, &file, case.root()), "normalized");
    });
}

fn complete_scope_case(py: Python<'_>, case: &Case, name: &str, expected: &str) {
    let root = case.mkdir(name);
    let mut relative = "pkg/test_contract.py";
    let mut file = root.join(relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "def test_contract():\n    assert True\n").unwrap();
    let mut nodeid = format!("{relative}::test_contract");
    let scope = PyDict::new(py);
    scope.set_item("mode", "complete").unwrap();
    scope.set_item("inventory", "python_ast").unwrap();
    scope
        .set_item("nodeids", PyList::new(py, [&nodeid]).unwrap())
        .unwrap();
    let mut ranked_row = ranked(py, 1, &nodeid, "contract", "rationale");
    let pins = PyDict::new(py);
    pins.set_item(relative, sha(py, &file)).unwrap();
    match name {
        "mode" => scope.set_item("mode", "unknown").unwrap(),
        "empty" => scope.set_item("nodeids", PyList::empty(py)).unwrap(),
        "duplicate" => scope
            .set_item("nodeids", PyList::new(py, [&nodeid, &nodeid]).unwrap())
            .unwrap(),
        "wrong_file" => scope
            .set_item(
                "nodeids",
                PyList::new(py, ["other/test_contract.py::test_contract"]).unwrap(),
            )
            .unwrap(),
        "rank_mismatch" => {
            ranked_row = ranked(
                py,
                1,
                &format!("{relative}::test_other"),
                "contract",
                "rationale",
            );
        }
        "unbound" => pins.clear(),
        "unsupported" => scope.set_item("inventory", "javascript_ast").unwrap(),
        "wrong_suffix" => {
            relative = "pkg/test_contract.js";
            file = root.join(relative);
            fs::write(&file, "test('contract', () => {});\n").unwrap();
            nodeid = format!("{relative}::test_contract");
            scope
                .set_item("nodeids", PyList::new(py, [&nodeid]).unwrap())
                .unwrap();
            ranked_row = ranked(py, 1, &nodeid, "contract", "rationale");
            pins.clear();
            pins.set_item(relative, sha(py, &file)).unwrap();
        }
        "syntax" => {
            fs::write(&file, "def test_contract(\n").unwrap();
            pins.set_item(relative, sha(py, &file)).unwrap();
        }
        "no_tests" => {
            fs::write(&file, "VALUE = 1\n").unwrap();
            pins.set_item(relative, sha(py, &file)).unwrap();
        }
        _ => panic!("unknown source row {name}"),
    }
    let scopes = PyDict::new(py);
    scopes.set_item(relative, scope).unwrap();
    let ranked_tests = PyTuple::new(py, [ranked_row]).unwrap();
    campaign_error_pattern(
        py,
        load_scopes(
            py,
            &root,
            scopes.as_any(),
            pins.as_any(),
            ranked_tests.as_any(),
        ),
        expected,
    );
}

#[test]
fn test_complete_scope_manifest_validation_fails_closed() {
    let case = Case::new();
    Python::attach(|py| {
        for (name, expected) in [
            ("mode", "mode must be"),
            ("empty", "may not be empty"),
            ("duplicate", "contains duplicates"),
            ("wrong_file", "another file"),
            ("rank_mismatch", "ranked_tests nodeids are missing"),
            ("unbound", "not bound"),
            ("unsupported", "inventory is unsupported"),
            ("wrong_suffix", "requires a .py file"),
            ("syntax", "cannot inventory Python tests"),
            ("no_tests", "scope is empty"),
        ] {
            complete_scope_case(py, &case, name, expected);
        }
    });
}

#[test]
fn test_pin_interpreter_resolves_bare_python_to_the_runner_interpreter() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = testing(py);
        let pin = subject.getattr("_pin_interpreter").unwrap();
        let sys = module(py, "sys");
        let runner = sys.getattr("executable").unwrap();
        for bare in ["python", "python3"] {
            let resolved = pin
                .call1((PyList::new(py, [bare, "-m", "pytest"]).unwrap(),))
                .unwrap();
            equal(&resolved.get_item(0).unwrap(), &runner);
        }
        let absolute = "/home/tim/venvs/llm/bin/python";
        let resolved = pin
            .call1((PyList::new(py, [absolute, "-m", "pytest"]).unwrap(),))
            .unwrap();
        assert_eq!(
            resolved.get_item(0).unwrap().extract::<String>().unwrap(),
            absolute
        );
        equal(
            &pin.call1((PyList::empty(py),)).unwrap(),
            &PyList::empty(py),
        );
    });
}
