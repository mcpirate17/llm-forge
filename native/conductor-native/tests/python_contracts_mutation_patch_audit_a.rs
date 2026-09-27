#![cfg(feature = "python-compat-tests")]
//! Mutation patch audit contracts 1–11: patch and receipt acceptance.

#[path = "python_contracts/mutation_audit_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture as f;
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict};
use serde_json::{json, Value};
use std::fs;
use support::{module, path, AttrPatch};

fn patch_corpus<'py>(
    py: Python<'py>,
    root: &std::path::Path,
    id: &str,
) -> (
    Bound<'py, pyo3::types::PyAny>,
    Bound<'py, pyo3::types::PyAny>,
    Bound<'py, pyo3::types::PyAny>,
) {
    let live = f::mutation(
        py,
        "live",
        &f::patch(root, "live", "VALUE = 1", "VALUE = 2"),
        None,
    );
    let rotted = f::mutation(
        py,
        "rotted",
        &f::patch(root, "rotted", "VALUE = 9", "VALUE = 2"),
        None,
    );
    let campaign = f::campaign(py, root, id, &[live.clone(), rotted.clone()]);
    (campaign, live, rotted)
}

#[test]
fn a_rotted_anchor_is_reported_and_a_live_patch_is_not() {
    let case = f::isolated_case();
    f::git_repo(case.root());
    Python::attach(|py| {
        let (campaign, live, rotted) = patch_corpus(py, case.root(), "corpus");
        let audit = f::audit(py);
        let check = audit.getattr("_patch_verdict").unwrap();
        assert!(check
            .call1((&campaign, live, path(py, case.root())))
            .unwrap()
            .is_none());
        let row = f::py_to_json(
            &check
                .call1((campaign, rotted, path(py, case.root())))
                .unwrap(),
        );
        assert_eq!(row["reason"], "DOES_NOT_APPLY");
        assert_eq!(row["mutation_id"], "rotted");
        let detail = row["detail"].as_str().unwrap();
        let (git_reason, anchored) = detail.split_once("; anchored retry: ").unwrap();
        assert!(git_reason.contains("source.py"));
        assert!(anchored.contains("source.py"));
    });
}

#[test]
fn the_check_runs_against_the_repo_root_not_the_process_directory() {
    let case = f::isolated_case();
    f::git_repo(case.root());
    let elsewhere = case.root().join("elsewhere");
    f::git_repo(&elsewhere);
    fs::write(elsewhere.join("source.py"), "VALUE = 9\n").unwrap();
    let _cwd = case.chdir("elsewhere");
    Python::attach(|py| {
        let (campaign, live, rotted) = patch_corpus(py, case.root(), "corpus");
        let check = f::audit(py).getattr("_patch_verdict").unwrap();
        assert!(check
            .call1((&campaign, &live, path(py, case.root())))
            .unwrap()
            .is_none());
        let stale = f::py_to_json(
            &check
                .call1((&campaign, rotted, path(py, case.root())))
                .unwrap(),
        );
        assert_eq!(stale["reason"], "DOES_NOT_APPLY");
        let elsewhere_row =
            f::py_to_json(&check.call1((campaign, live, path(py, &elsewhere))).unwrap());
        assert_eq!(elsewhere_row["reason"], "DOES_NOT_APPLY");
    });
}

#[test]
fn a_missing_or_drifted_patch_is_reported_without_being_applied() {
    let case = f::isolated_case();
    f::git_repo(case.root());
    Python::attach(|py| {
        let absent_file = f::patch(case.root(), "absent", "VALUE = 1", "VALUE = 2");
        let absent = f::mutation(py, "absent", &absent_file, None);
        fs::remove_file(&absent_file).unwrap();
        let drifted_file = f::patch(case.root(), "drifted", "VALUE = 1", "VALUE = 2");
        let drifted = f::mutation(py, "drifted", &drifted_file, Some(&"0".repeat(64)));
        let campaign = f::campaign(
            py,
            case.root(),
            "corpus",
            &[absent.clone(), drifted.clone()],
        );
        let check = f::audit(py).getattr("_patch_verdict").unwrap();
        let missing = f::py_to_json(
            &check
                .call1((&campaign, absent, path(py, case.root())))
                .unwrap(),
        );
        assert_eq!(missing["reason"], "MISSING");
        assert!(missing["detail"]
            .as_str()
            .unwrap()
            .contains("no patch file at"));
        let drift = f::py_to_json(
            &check
                .call1((campaign, drifted, path(py, case.root())))
                .unwrap(),
        );
        assert_eq!(drift["reason"], "HASH_DRIFT");
        assert!(drift["detail"].as_str().unwrap().contains(&"0".repeat(64)));
    });
}

fn registered_corpus(py: Python<'_>, case: &support::Case, loadable: bool) -> Value {
    let root = case.root();
    f::git_repo(root);
    let (campaign, _, _) = patch_corpus(py, root, "loadable");
    let entries = if loadable {
        json!([{"manifest":"broken.json"}, {"manifest":"loadable.json"}])
    } else {
        json!([{"manifest":"broken.json"}])
    };
    let registry = json!({"campaigns": entries});
    let registry_file = root.join("registry.json");
    fs::write(&registry_file, registry.to_string()).unwrap();
    let audit = f::audit(py);
    let _registry_patch = f::patch_constant(py, "_load_registry", &f::json_to_py(py, &registry));
    let held = campaign.unbind();
    let loader = PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
        let manifest = args.get_item(0)?;
        let name: String = manifest.getattr("name")?.extract()?;
        if name == "broken.json" {
            let class = module(args.py(), "conductor.mutation_scope").getattr("CampaignError")?;
            let message = if loadable {
                "manifest is not a mapping"
            } else {
                "unreadable"
            };
            return Err(PyErr::from_value(class.call1((message,))?));
        }
        Ok(held.clone_ref(args.py()))
    })
    .unwrap();
    let _loader_patch = AttrPatch::replace(&audit, "load_campaign", loader.as_any());
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo_root", path(py, root)).unwrap();
    f::py_to_json(
        &audit
            .getattr("audit_patches")
            .unwrap()
            .call((path(py, &registry_file),), Some(&kwargs))
            .unwrap(),
    )
}

#[test]
fn one_unloadable_manifest_does_not_hide_the_rest_of_the_corpus() {
    let case = f::isolated_case();
    let result = Python::attach(|py| registered_corpus(py, &case, true));
    assert_eq!(result["campaigns"], 1);
    assert_eq!(result["mutations"], 2);
    assert_eq!(result["stale_mutations"], 1);
    assert_eq!(result["stale_campaigns"], json!({"loadable": 1}));
    assert_eq!(result["stale"][0]["mutation_id"], "rotted");
    assert_eq!(
        result["unloadable"],
        json!([{"manifest":"broken.json", "detail":"manifest is not a mapping"}])
    );
    assert_eq!(result["status"], "STALE");
}

#[test]
fn a_corpus_that_only_fails_to_load_is_not_reported_clean() {
    let case = f::isolated_case();
    let result = Python::attach(|py| registered_corpus(py, &case, false));
    assert_eq!(result["stale_mutations"], 0);
    assert_eq!(result["status"], "STALE");
}

#[test]
fn an_absolute_interpreter_is_reported_and_a_bare_one_is_not() {
    let case = f::isolated_case();
    f::git_repo(case.root());
    Python::attach(|py| {
        let campaign = f::campaign(py, case.root(), "bare", &[]);
        let verdict = f::audit(py).getattr("_interpreter_verdict").unwrap();
        assert!(verdict
            .call1((&campaign, path(py, case.root())))
            .unwrap()
            .is_none());
        let present = case.root().join("venv/bin/python");
        fs::create_dir_all(present.parent().unwrap()).unwrap();
        fs::write(&present, "").unwrap();
        let pinned = f::replace(
            py,
            &campaign,
            &json!({"campaign_id":"pinned", "test_argv":[present.to_str().unwrap(),"-m","pytest"]}),
        );
        let row = f::py_to_json(&verdict.call1((pinned, path(py, case.root()))).unwrap());
        assert_eq!(row["reason"], "INTERPRETER_HOST_PINNED");
        assert_eq!(row["interpreter"], present.to_str().unwrap());
        assert_eq!(row["campaign_id"], "pinned");
        for fragment in [
            "runner's own interpreter",
            "absolute path",
            "checkout",
            "use a bare `python`",
        ] {
            assert!(row["detail"].as_str().unwrap().contains(fragment));
        }
        let absent = f::replace(
            py,
            &campaign,
            &json!({"campaign_id":"absent", "test_argv":[case.root().join("gone/python").to_str().unwrap(),"-m","pytest"]}),
        );
        let row = f::py_to_json(&verdict.call1((absent, path(py, case.root()))).unwrap());
        assert_eq!(row["reason"], "INTERPRETER_ABSENT");
        assert!(row["detail"]
            .as_str()
            .unwrap()
            .contains("does not exist on this host"));
    });
}

#[test]
fn receipts_are_indexed_by_their_declared_id_not_their_filename() {
    let case = f::isolated_case();
    for (name, payload) in [
        ("run-a", json!({"campaign_id":"corpus", "status":"PASS"})),
        ("run-b", json!({"campaign_id":"corpus", "status":"FAIL"})),
        ("other", json!({"campaign_id":"elsewhere", "status":"PASS"})),
    ] {
        f::receipt(case.root(), name, &payload);
    }
    fs::write(case.root().join("receipts/junk.json"), "{not json").unwrap();
    fs::write(case.root().join("receipts/wrong-shape.json"), "[]").unwrap();
    Python::attach(|py| {
        let read = f::audit(py).getattr("_receipts_by_campaign").unwrap();
        let index = f::py_to_json(&read.call1((path(py, case.root()), ["receipts"])).unwrap());
        assert_eq!(
            index
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            ["corpus", "elsewhere"].into_iter().collect()
        );
        assert_eq!(index["corpus"].as_array().unwrap().len(), 2);
        let statuses: std::collections::BTreeSet<_> = index["corpus"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["PASS", "FAIL"].into_iter().collect());
        assert_eq!(
            f::py_to_json(
                &read
                    .call1((path(py, case.root()), ["absent", "receipts"]))
                    .unwrap()
            ),
            index
        );
    });
}

#[test]
fn a_receipt_is_evidence_only_when_a_known_runner_produced_it() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let current = f::current();
        let campaign = f::campaign(py, case.root(), "corpus", &[]);
        let check = f::audit(py).getattr("_receipt_rejection").unwrap();
        for (receipt, expected) in [
            (
                json!({"status":"PASS", "runner_components_sha256": current}),
                None,
            ),
            (
                json!({"status":"BASELINE_FAILED", "runner_components_sha256": current}),
                Some("status=BASELINE_FAILED"),
            ),
            (json!({"status":"PASS"}), Some("no runner component map")),
            (
                json!({"status":"PASS", "runner_components_sha256":{"conductor/mutation_testing.py":"b".repeat(64)}}),
                Some("runner components match neither this runner nor any lineage entry"),
            ),
            (
                json!({"status":"RATCHET_HELD", "runner_components_sha256": current}),
                None,
            ),
            (
                json!({"status":"RATCHET_HELD", "runner_components_sha256":{"conductor/mutation_testing.py":"b".repeat(64)}}),
                Some("runner components match neither this runner nor any lineage entry"),
            ),
        ] {
            let actual = check
                .call1((
                    f::json_to_py(py, &receipt),
                    f::json_to_py(py, &current),
                    path(py, case.root()),
                    f::tree(py, case.root()),
                    &campaign,
                ))
                .unwrap();
            assert_eq!(
                actual.extract::<Option<String>>().unwrap().as_deref(),
                expected,
                "receipt={receipt}"
            );
        }
        let statuses: std::collections::BTreeSet<String> = f::audit(py)
            .getattr("PASSING_RECEIPT_STATUSES")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            statuses,
            ["PASS".to_owned(), "RATCHET_HELD".to_owned()].into()
        );
    });
}

#[test]
fn one_acceptable_receipt_covers_a_campaign_and_none_leaves_it_uncovered() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let current = f::current();
        let campaign = f::campaign(py, case.root(), "corpus", &[]);
        let good = json!({"status":"PASS", "runner_components_sha256":current});
        let stale = json!({"status":"PASS", "runner_components_sha256":{"conductor/mutation_testing.py":"b".repeat(64)}});
        let verdict = f::audit(py).getattr("_evidence_verdict").unwrap();
        let arguments = |rows: Value| {
            (
                campaign.clone(),
                f::json_to_py(py, &rows),
                f::json_to_py(py, &current),
                path(py, case.root()),
                f::tree(py, case.root()),
            )
        };
        assert!(verdict
            .call1(arguments(json!({"corpus":[stale, good]})))
            .unwrap()
            .is_none());
        let missing = f::py_to_json(&verdict.call1(arguments(json!({}))).unwrap());
        assert_eq!(missing["reason"], "NO_RECEIPT");
        assert_eq!(missing["campaign_id"], "corpus");
        assert_eq!(missing["receipts"], 0);
        assert_eq!(missing["detail"], "no receipt declares this campaign_id");
        let unusable = f::py_to_json(
            &verdict
                .call1(arguments(json!({"corpus":[stale, stale]})))
                .unwrap(),
        );
        assert_eq!(unusable["reason"], "NO_ACCEPTABLE_RECEIPT");
        assert_eq!(unusable["campaign_id"], "corpus");
        assert!(unusable["detail"]
            .as_str()
            .unwrap()
            .contains("runner components"));
        assert_eq!(unusable["receipts"], 2);
    });
}

#[test]
fn a_campaign_is_unmeasured_or_its_inert_tests_are_named() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let current = f::current();
        let campaign = f::campaign(py, case.root(), "unmeasured", &[]);
        let measured = f::measured(py, &f::campaign(py, case.root(), "measured", &[]));
        let value = json!({"tests":[{"nodeid":"t.py::test_kills", "classification":"CORE"}, {"nodeid":"t.py::test_inert", "classification":"DELETE_CANDIDATE"}]});
        let receipt = f::value_receipt(&current, value.clone(), "20260905T000000Z");
        let (unmeasured, inert) = f::value_verdicts(
            py,
            case.root(),
            &[campaign.clone(), measured.clone()],
            &json!({"measured":[receipt]}),
            &current,
        );
        assert_eq!(unmeasured[0]["campaign_id"], "unmeasured");
        assert_eq!(unmeasured[0]["reason"], "NO_VALUE_ANALYSIS");
        assert!(unmeasured[0]["detail"]
            .as_str()
            .unwrap()
            .contains("nothing measures"));
        assert_eq!(inert[0]["nodeid"], "t.py::test_inert");
        assert_eq!(inert[0]["reason"], "KILLS_NOTHING");
        let stale = f::value_receipt(
            &json!({"conductor/mutation_testing.py":"b".repeat(64)}),
            value.clone(),
            "20260905T000000Z",
        );
        assert_eq!(
            f::value_verdicts(
                py,
                case.root(),
                &[measured],
                &json!({"measured":[stale]}),
                &current
            ),
            (json!([]), json!([]))
        );
        let (unmeasured, inert) = f::value_verdicts(
            py,
            case.root(),
            std::slice::from_ref(&campaign),
            &json!({"unmeasured":[f::value_receipt(&current, value, "20260908T000000Z")]}),
            &current,
        );
        assert_eq!(unmeasured, json!([]));
        assert_eq!(inert[0]["nodeid"], "t.py::test_inert");
        let generated = f::replace(py, &campaign, &json!({"generated":true}));
        let (missing, inert) = f::value_verdicts(
            py,
            case.root(),
            std::slice::from_ref(&generated),
            &json!({"unmeasured":[f::value_receipt(&current, Value::Null, "20260908T000000Z")]}),
            &current,
        );
        assert_eq!((missing, inert), (json!([]), json!([])));
        let (invalid, inert) = f::value_verdicts(
            py,
            case.root(),
            &[generated],
            &json!({"unmeasured":[f::value_receipt(&current, json!({"tests":[{"nodeid":"t.py::bad"}]}), "20260908T000000Z")]}),
            &current,
        );
        assert_eq!(invalid[0]["reason"], "INVALID_VALUE_ANALYSIS");
        assert!(invalid[0]["detail"]
            .as_str()
            .unwrap()
            .contains("nodeid and classification"));
        assert_eq!(inert, json!([]));
    });
}

#[test]
fn the_newest_acceptable_receipt_is_the_one_that_speaks() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let current = f::current();
        let campaign = f::campaign(py, case.root(), "corpus", &[]);
        let value = json!({"tests":[{"nodeid":"t.py::a", "classification":"CORE"}]});
        let old = f::value_receipt(&current, value.clone(), "20260901T000000Z");
        let new = f::value_receipt(&current, value, "20260905T000000Z");
        let choose = f::audit(py).getattr("_acceptable_receipt").unwrap();
        let args = |receipts: Value| {
            (
                campaign.clone(),
                f::json_to_py(py, &receipts),
                f::json_to_py(py, &current),
                path(py, case.root()),
                f::tree(py, case.root()),
            )
        };
        let chosen = f::py_to_json(&choose.call1(args(json!({"corpus":[new, old]}))).unwrap());
        assert_eq!(chosen["generated_at"], "20260905T000000Z");
        assert!(choose.call1(args(json!({}))).unwrap().is_none());
    });
}
