#![cfg(feature = "python-compat-tests")]
//! Mutation audit contracts 12–19: baseline dimensions and CLI verdicts.

#[path = "python_contracts/mutation_audit_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture as f;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use support::{module, path, AttrPatch};

fn baseline_sets<'py>(py: Python<'py>, entries: &Value) -> Bound<'py, PyAny> {
    let result = PyDict::new(py);
    let keys: Vec<String> = f::audit(py)
        .getattr("BASELINE_KEYS")
        .unwrap()
        .extract()
        .unwrap();
    let set = module(py, "builtins").getattr("set").unwrap();
    for key in keys {
        let values = entries.get(&key).cloned().unwrap_or_else(|| json!([]));
        result
            .set_item(key, set.call1((f::json_to_py(py, &values),)).unwrap())
            .unwrap();
    }
    result.into_any()
}

fn sorted_field(object: &Bound<'_, PyAny>, key: &str) -> Value {
    let py = object.py();
    let rows = module(py, "builtins")
        .getattr("sorted")
        .unwrap()
        .call1((object.get_item(key).unwrap(),))
        .unwrap();
    f::py_to_json(&rows)
}

fn audit_fixture(root: &Path) -> (Value, Value) {
    let patches = json!({
        "status":"STALE", "repo_root":root.to_str().unwrap(), "campaigns":1,
        "mutations":1, "stale_mutations":1, "stale_campaigns":{"corpus":1},
        "stale":[{"campaign_id":"corpus","mutation_id":"rotted"}], "unloadable":[]
    });
    let repro = json!({
        "campaigns":1, "host_pinned_interpreters":1, "uncovered_campaigns":0,
        "campaigns_without_value_analysis":1, "tests_that_kill_nothing":1,
        "uncovered_changed_files":0, "interpreters":[{"campaign_id":"pinned"}],
        "evidence":[], "unmeasured":[{"campaign_id":"unmeasured"}],
        "inert_tests":[{"campaign_id":"corpus","nodeid":"t.py::test_x"}],
        "uncovered_files":[]
    });
    (patches, repro)
}

fn main_patches(py: Python<'_>, patches: &Value, repro: &Value) -> Vec<AttrPatch> {
    let empty = PyTuple::new(
        py,
        [
            PyDict::new(py).into_any(),
            PyList::empty(py).into_any(),
            PyList::empty(py).into_any(),
        ],
    )
    .unwrap()
    .into_any();
    vec![
        f::patch_constant(py, "load_registered_campaigns", &empty),
        f::patch_constant(py, "audit_patches", &f::json_to_py(py, patches)),
        f::patch_constant(py, "audit_reproducibility", &f::json_to_py(py, repro)),
    ]
}

fn main_with_baseline(py: Python<'_>, baseline: &Path, extra: &[&str]) -> (i32, Value) {
    let mut args = vec![
        "--baseline".to_owned(),
        baseline.to_str().unwrap().to_owned(),
    ];
    args.extend(extra.iter().map(|s| (*s).to_owned()));
    f::main(py, &args)
}

#[test]
fn every_dimension_reduces_to_comparable_ids() {
    let _case = f::isolated_case();
    Python::attach(|py| {
        let patches = json!({"stale":[
            {"campaign_id":"one","mutation_id":"rotted"},
            {"campaign_id":"two","mutation_id":"rotted"}],
            "unloadable":[{"manifest":"broken.json"}]});
        let repro = json!({
            "interpreters":[{"campaign_id":"pinned"}],
            "evidence":[{"campaign_id":"uncovered"}],
            "unmeasured":[{"campaign_id":"unmeasured"}],
            "inert_tests":[
                {"campaign_id":"one","nodeid":"t.py::test_x"},
                {"campaign_id":"two","nodeid":"t.py::test_x"}],
            "uncovered_files":[{"file":"src/new.py"}]
        });
        let found = f::audit(py)
            .getattr("_findings")
            .unwrap()
            .call1((f::json_to_py(py, &patches), f::json_to_py(py, &repro)))
            .unwrap();
        for (key, expected) in [
            ("stale_mutations", json!(["one::rotted", "two::rotted"])),
            ("unloadable_manifests", json!(["broken.json"])),
            ("host_pinned_interpreters", json!(["pinned"])),
            ("uncovered_campaigns", json!(["uncovered"])),
            ("campaigns_without_value_analysis", json!(["unmeasured"])),
            ("uncovered_changed_files", json!(["src/new.py"])),
            (
                "tests_that_kill_nothing",
                json!(["one::t.py::test_x", "two::t.py::test_x"]),
            ),
        ] {
            assert_eq!(sorted_field(&found, key), expected);
        }
        let found_keys: Vec<String> = module(py, "builtins")
            .getattr("sorted")
            .unwrap()
            .call1((found.call_method0("keys").unwrap(),))
            .unwrap()
            .extract()
            .unwrap();
        let mut baseline_keys: Vec<String> = f::audit(py)
            .getattr("BASELINE_KEYS")
            .unwrap()
            .extract()
            .unwrap();
        baseline_keys.sort();
        assert_eq!(found_keys, baseline_keys);
    });
}

#[test]
fn the_baseline_ratchet_bites_in_both_directions() {
    let _case = f::isolated_case();
    Python::attach(|py| {
        let found = baseline_sets(
            py,
            &json!({
                "stale_mutations":["one::rotted"],
                "host_pinned_interpreters":["pinned"],
                "uncovered_campaigns":["uncovered"]
            }),
        );
        let delta = f::audit(py).getattr("_baseline_delta").unwrap();
        let clean = f::py_to_json(&delta.call1((&found, &found)).unwrap());
        assert_eq!(clean["status"], "CLEAN");
        assert_eq!(clean["new_host_pinned_interpreters"], json!([]));
        assert_eq!(clean["resolved_uncovered_campaigns"], json!([]));
        let recorded = baseline_sets(
            py,
            &json!({
                "stale_mutations":["one::rotted"], "uncovered_campaigns":["uncovered"]
            }),
        );
        let regressed = f::py_to_json(&delta.call1((&found, recorded)).unwrap());
        assert_eq!(regressed["status"], "REGRESSED");
        assert_eq!(regressed["new_host_pinned_interpreters"], json!(["pinned"]));
        let recorded = baseline_sets(
            py,
            &json!({
                "stale_mutations":["one::rotted"],
                "host_pinned_interpreters":["pinned"],
                "uncovered_campaigns":["uncovered", "already_fixed"]
            }),
        );
        let stale = f::py_to_json(&delta.call1((found, recorded)).unwrap());
        assert_eq!(stale["status"], "BASELINE_STALE");
        assert_eq!(
            stale["resolved_uncovered_campaigns"],
            json!(["already_fixed"])
        );
    });
}

#[test]
fn a_missing_baseline_makes_every_finding_new() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let audit = f::audit(py);
        let recorded = audit
            .getattr("_load_baseline")
            .unwrap()
            .call1((path(py, Path::new("absent.json")), path(py, case.root())))
            .unwrap();
        let found = baseline_sets(py, &json!({"host_pinned_interpreters":["pinned"]}));
        let delta = f::py_to_json(
            &audit
                .getattr("_baseline_delta")
                .unwrap()
                .call1((found, recorded))
                .unwrap(),
        );
        assert_eq!(delta["status"], "REGRESSED");
    });
}

#[test]
fn the_exit_code_and_summary_carry_the_verdict() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let (patches, repro) = audit_fixture(case.root());
        let _patches = main_patches(py, &patches, &repro);
        let baseline = case.root().join("baseline.json");
        f::baseline(
            case.root(),
            &BTreeMap::from([
                ("stale_mutations", vec!["corpus::rotted"]),
                ("host_pinned_interpreters", vec!["pinned"]),
                ("campaigns_without_value_analysis", vec!["unmeasured"]),
                ("tests_that_kill_nothing", vec!["corpus::t.py::test_x"]),
            ]),
        );
        let (rc, summary) = main_with_baseline(py, &baseline, &["--summary"]);
        assert_eq!(rc, 0);
        assert_eq!(summary["status"], "CLEAN");
        assert_eq!(summary["repo_root"], case.root().to_str().unwrap());
        assert_eq!(summary["campaigns"], 1);
        assert!(summary["patches"].get("stale").is_none());
        for key in [
            "interpreters",
            "unmeasured",
            "inert_tests",
            "uncovered_files",
        ] {
            assert!(summary["reproducibility"].get(key).is_none());
        }
        assert_eq!(summary["patches"]["stale_mutations"], 1);
        assert_eq!(summary["patches"]["stale_campaigns"], json!({"corpus":1}));
        assert_eq!(
            summary["patches"]["repo_root"],
            case.root().to_str().unwrap()
        );
        assert_eq!(summary["patches"]["campaigns"], 1);
        assert_eq!(summary["patches"]["mutations"], 1);
        assert_eq!(summary["reproducibility"]["host_pinned_interpreters"], 1);
        assert_eq!(
            summary["reproducibility"]["campaigns_without_value_analysis"],
            1
        );
        assert_eq!(summary["reproducibility"]["tests_that_kill_nothing"], 1);
        assert_eq!(summary["reproducibility"]["uncovered_changed_files"], 0);

        f::baseline(
            case.root(),
            &BTreeMap::from([
                ("stale_mutations", vec!["corpus::rotted"]),
                ("host_pinned_interpreters", vec!["pinned"]),
                ("campaigns_without_value_analysis", vec!["unmeasured"]),
            ]),
        );
        let (rc, report) = main_with_baseline(py, &baseline, &[]);
        assert_eq!(rc, 7);
        assert_eq!(
            report["reproducibility"]["baseline"]["new_tests_that_kill_nothing"],
            json!(["corpus::t.py::test_x"])
        );

        f::baseline(
            case.root(),
            &BTreeMap::from([("host_pinned_interpreters", vec!["pinned"])]),
        );
        assert_eq!(main_with_baseline(py, &baseline, &[]).0, 6);
        f::baseline(
            case.root(),
            &BTreeMap::from([("stale_mutations", vec!["corpus::rotted"])]),
        );
        let (rc, report) = main_with_baseline(py, &baseline, &[]);
        assert_eq!(rc, 7);
        assert_eq!(report["status"], "REGRESSED");
        assert_eq!(report["patches"], patches);
        assert_eq!(
            report["reproducibility"]["baseline"]["new_host_pinned_interpreters"],
            json!(["pinned"])
        );
    });
}

#[test]
fn write_baseline_records_instead_of_judging() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let patches = json!({"status":"CLEAN", "repo_root":case.root().to_str().unwrap(),
            "campaigns":0,"stale":[{"campaign_id":"corpus","mutation_id":"rotted"}],"unloadable":[]});
        let repro = json!({"interpreters":[{"campaign_id":"pinned"}],
            "evidence":[{"campaign_id":"uncovered"}],
            "unmeasured":[{"campaign_id":"unmeasured"}],
            "inert_tests":[{"campaign_id":"corpus","nodeid":"t.py::test_x"}],
            "uncovered_files":[]});
        let _patches = main_patches(py, &patches, &repro);
        let baseline = case.root().join("baseline.json");
        let (rc, _) = main_with_baseline(py, &baseline, &["--write-baseline", "--summary"]);
        assert_eq!(rc, 0);
        let bytes = fs::read(&baseline).unwrap();
        assert!(bytes.ends_with(b"\n"));
        let recorded: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(recorded["schema_version"], 1);
        assert!(recorded["note"].as_str().unwrap().contains("ratchet"));
        for (key, expected) in [
            ("stale_mutations", "corpus::rotted"),
            ("host_pinned_interpreters", "pinned"),
            ("uncovered_campaigns", "uncovered"),
            ("campaigns_without_value_analysis", "unmeasured"),
            ("tests_that_kill_nothing", "corpus::t.py::test_x"),
        ] {
            assert_eq!(recorded[key], json!([expected]));
        }
    });
}

#[test]
fn optional_attribution_rejects_each_malformed_shape() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let campaign = f::replace(
            py,
            &f::campaign(py, case.root(), "generated", &[]),
            &json!({"generated":true}),
        );
        let current = json!({"runner":"digest"});
        let inert_row = json!({"nodeid":"t.py::inert","classification":"DELETE_CANDIDATE"});
        for value in [
            json!([]),
            json!({"tests":"not a list"}),
            json!({"tests":[null]}),
            json!({"tests":[{"nodeid":1,"classification":"CORE"}]}),
            json!({"tests":[{"nodeid":"t.py::bad","classification":null},inert_row]}),
        ] {
            let receipt =
                json!({"status":"PASS","runner_components_sha256":current,"test_value":value});
            let (unmeasured, inert) = f::value_verdicts(
                py,
                case.root(),
                std::slice::from_ref(&campaign),
                &json!({"generated":[receipt]}),
                &current,
            );
            assert_eq!(unmeasured.as_array().unwrap().len(), 1);
            assert_eq!(unmeasured[0]["campaign_id"], "generated");
            assert_eq!(unmeasured[0]["reason"], "INVALID_VALUE_ANALYSIS");
            assert_eq!(inert, json!([]));
        }
        let good = json!({"status":"PASS","runner_components_sha256":current,"test_value":{"tests":[inert_row]}});
        let (unmeasured, inert) = f::value_verdicts(
            py,
            case.root(),
            std::slice::from_ref(&campaign),
            &json!({"generated":[good.clone()]}),
            &current,
        );
        assert_eq!(unmeasured, json!([]));
        assert_eq!(
            inert,
            json!([{"campaign_id":"generated","nodeid":"t.py::inert","reason":"KILLS_NOTHING"}])
        );
        let second = f::replace(py, &campaign, &json!({"campaign_id":"second"}));
        let invalid = json!({"status":"PASS","runner_components_sha256":current,"test_value":[]});
        let (unmeasured, inert) = f::value_verdicts(
            py,
            case.root(),
            &[campaign, second],
            &json!({"generated":[invalid],"second":[good]}),
            &current,
        );
        assert_eq!(
            unmeasured,
            json!([{"campaign_id":"generated","reason":"INVALID_VALUE_ANALYSIS","detail":"receipt test_value must contain a tests list"}])
        );
        assert_eq!(
            inert,
            json!([{"campaign_id":"second","nodeid":"t.py::inert","reason":"KILLS_NOTHING"}])
        );
    });
}

#[test]
fn receipt_and_baseline_io_validate_shapes_and_preserve_later_rows() {
    let case = f::isolated_case();
    f::receipt(case.root(), "00-malformed", &json!([]));
    f::receipt(case.root(), "01-valid", &json!({"campaign_id":"later"}));
    Python::attach(|py| {
        let audit = f::audit(py);
        let receipts = audit.getattr("_receipts_by_campaign").unwrap();
        let rows = f::py_to_json(
            &receipts
                .call1((path(py, case.root()), ["receipts"]))
                .unwrap(),
        );
        assert_eq!(rows, json!({"later":[{"campaign_id":"later"}]}));
        let error = receipts
            .call1((path(py, case.root()), ["../outside"]))
            .unwrap_err();
        assert!(error.to_string().contains("registry.receipt_directories"));
        let baseline = case.root().join("baseline.json");
        let key: Vec<String> = audit.getattr("BASELINE_KEYS").unwrap().extract().unwrap();
        for invalid in [json!("not a list"), json!([1])] {
            fs::write(&baseline, json!({key[0].clone(): invalid}).to_string()).unwrap();
            let error = audit
                .getattr("_load_baseline")
                .unwrap()
                .call1((path(py, &baseline), path(py, case.root())))
                .unwrap_err();
            assert!(error.to_string().contains("must be an array of ids"));
        }
        audit
            .getattr("_write_baseline")
            .unwrap()
            .call1((
                path(py, Path::new("relative.json")),
                path(py, case.root()),
                baseline_sets(py, &json!({})),
            ))
            .unwrap();
        assert!(fs::read(case.root().join("relative.json"))
            .unwrap()
            .ends_with(b"\n"));
    });
}

#[test]
fn empty_patch_corpus_is_clean_and_external_interpreter_is_identified() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let audit = f::audit(py);
        let loaded = PyTuple::new(
            py,
            [
                PyDict::new(py).into_any(),
                PyList::empty(py).into_any(),
                PyList::empty(py).into_any(),
            ],
        )
        .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("repo_root", path(py, case.root())).unwrap();
        kwargs.set_item("loaded", loaded).unwrap();
        let result = f::py_to_json(
            &audit
                .getattr("audit_patches")
                .unwrap()
                .call((path(py, Path::new("registry.json")),), Some(&kwargs))
                .unwrap(),
        );
        assert_eq!(result["status"], "CLEAN");
        assert_eq!(result["repo_root"], case.root().to_str().unwrap());
        assert_eq!(result["campaigns"], 0);
        assert_eq!(result["mutations"], 0);
        let interpreter: String = module(py, "sys")
            .getattr("executable")
            .unwrap()
            .extract()
            .unwrap();
        let campaign = f::replace(
            py,
            &f::campaign(py, case.root(), "external", &[]),
            &json!({"test_argv":[interpreter,"-m","pytest"]}),
        );
        let verdict = f::py_to_json(
            &audit
                .getattr("_interpreter_verdict")
                .unwrap()
                .call1((campaign, path(py, case.root())))
                .unwrap(),
        );
        assert!(verdict["detail"]
            .as_str()
            .unwrap()
            .contains("host's filesystem"));
    });
}
