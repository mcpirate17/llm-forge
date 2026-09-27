#![cfg(feature = "python-compat-tests")]
//! Mutation audit contracts 20–27: evidence bytes, territory, and slim receipts.

#[path = "python_contracts/mutation_audit_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture as f;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyTuple};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use support::{module, path, AttrPatch};

fn evidence_env(py: Python<'_>, root: &Path) -> (Value, Vec<AttrPatch>) {
    let current = f::current();
    let patches = vec![
        f::patch_constant(py, "runner_component_root", &path(py, root)),
        f::patch_constant(
            py,
            "_runner_components_sha256",
            &f::json_to_py(py, &current),
        ),
    ];
    (current, patches)
}

fn repro(py: Python<'_>, root: &Path, campaigns: &[Bound<'_, PyAny>]) -> Value {
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo_root", path(py, root)).unwrap();
    f::py_to_json(
        &f::audit(py)
            .getattr("audit_reproducibility")
            .unwrap()
            .call(
                (
                    PyList::new(py, campaigns).unwrap(),
                    f::json_to_py(py, &json!({"receipt_directories":["receipts"]})),
                ),
                Some(&kwargs),
            )
            .unwrap(),
    )
}

fn receipt_rejection(
    py: Python<'_>,
    root: &Path,
    campaign: &Bound<'_, PyAny>,
    receipt: &Value,
    current: &Value,
) -> Option<String> {
    f::audit(py)
        .getattr("_receipt_rejection")
        .unwrap()
        .call1((
            f::json_to_py(py, receipt),
            f::json_to_py(py, current),
            path(py, root),
            f::tree(py, root),
            campaign,
        ))
        .unwrap()
        .extract()
        .unwrap()
}

fn changed_rows(
    py: Python<'_>,
    root: &Path,
    changed: &[&str],
    campaigns: &[Bound<'_, PyAny>],
) -> Value {
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo_root", path(py, root)).unwrap();
    f::py_to_json(
        &f::audit(py)
            .getattr("_uncovered_changed_files")
            .unwrap()
            .call(
                (changed, PyList::new(py, campaigns).unwrap()),
                Some(&kwargs),
            )
            .unwrap(),
    )
}

fn touch(root: &Path, relatives: &[&str]) {
    for relative in relatives {
        let file = root.join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "").unwrap();
    }
}

// Keep the production corpus algorithm intact while directing its tree reads
// to the isolated candidate tree used by these integration contracts.
fn patch_corpus_root(py: Python<'_>, root: &Path) -> AttrPatch {
    let audit = f::audit(py);
    let original = audit.getattr("audit_corpus").unwrap().unbind();
    let root = path(py, root).unbind();
    let wrapper =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            let py = args.py();
            let merged = PyDict::new(py);
            if let Some(kwargs) = kwargs {
                for (key, value) in kwargs.iter() {
                    merged.set_item(key, value)?;
                }
            }
            merged.set_item("repo_root", root.bind(py))?;
            Ok(original.bind(py).call(args, Some(&merged))?.unbind())
        })
        .unwrap();
    AttrPatch::replace(audit.as_any(), "audit_corpus", wrapper.as_any())
}

#[test]
fn audit_reproducibility_resolves_lineage_from_runner_component_root() {
    let case = f::isolated_case();
    let repo = case.root().join("repo");
    let package = case.root().join("elsewhere/src");
    fs::create_dir_all(repo.join("receipts")).unwrap();
    fs::create_dir_all(package.join("conductor")).unwrap();
    let recorded = json!({"conductor/mutation_testing.py":"b".repeat(64)});
    let current = f::current();
    fs::write(
        package.join("conductor/mutation_runner_lineage.json"),
        json!({"schema_version":1,"entries":[{"runner_components_sha256":recorded}]}).to_string(),
    )
    .unwrap();
    assert!(!repo.join("conductor/mutation_runner_lineage.json").exists());
    f::receipt(
        &repo,
        "corpus",
        &json!({"campaign_id":"corpus","status":"PASS","runner_components_sha256":recorded}),
    );
    Python::attach(|py| {
        let campaign = f::campaign(py, &repo, "corpus", &[]);
        let _current = f::patch_constant(
            py,
            "_runner_components_sha256",
            &f::json_to_py(py, &current),
        );
        let package_root = f::patch_constant(py, "runner_component_root", &path(py, &package));
        let good = repro(py, &repo, std::slice::from_ref(&campaign));
        assert_eq!(good["uncovered_campaigns"], 0);
        assert_eq!(good["evidence"], json!([]));
        drop(package_root);
        let _repo_root = f::patch_constant(py, "runner_component_root", &path(py, &repo));
        let bad = repro(py, &repo, &[campaign]);
        assert_eq!(bad["uncovered_campaigns"], 1);
        assert_eq!(bad["evidence"][0]["reason"], "NO_ACCEPTABLE_RECEIPT");
    });
}

#[test]
fn a_receipt_pinning_other_bytes_is_not_evidence() {
    let case = f::isolated_case();
    f::git_repo(case.root());
    Python::attach(|py| {
        let (current, _env) = evidence_env(py, case.root());
        let digest = f::sha256(py, &case.root().join("source.py"));
        let campaign = f::campaign(py, case.root(), "corpus", &[]);
        let fresh = json!({"campaign_id":"corpus","status":"PASS",
            "runner_components_sha256":current,"source_sha256":{"source.py":digest}});
        let mut stale = fresh.clone();
        stale["source_sha256"] = json!({"source.py":"b".repeat(64)});
        assert_eq!(
            receipt_rejection(py, case.root(), &campaign, &stale, &current),
            Some("source hashes differ from this tree: ['source.py']".to_owned())
        );
        assert_eq!(
            receipt_rejection(py, case.root(), &campaign, &fresh, &current),
            None
        );
        let mut absent = fresh.clone();
        absent["source_sha256"] = json!({"gone.py":digest});
        assert!(
            receipt_rejection(py, case.root(), &campaign, &absent, &current)
                .unwrap()
                .contains("gone.py")
        );
        f::receipt(case.root(), "stale", &stale);
        let result = repro(py, case.root(), std::slice::from_ref(&campaign));
        assert_eq!(result["uncovered_campaigns"], 1);
        assert_eq!(result["evidence"][0]["reason"], "NO_ACCEPTABLE_RECEIPT");
        assert!(result["evidence"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("source hashes differ"));
        let symbol = f::replace(
            py,
            &campaign,
            &json!({"source_symbols":{"source.py":{"VALUE":digest}}}),
        );
        assert_eq!(
            receipt_rejection(py, case.root(), &symbol, &stale, &current),
            None
        );
    });
}

#[test]
fn one_stale_hash_fails_the_whole_audit() {
    let case = f::isolated_case();
    f::git_repo(case.root());
    Python::attach(|py| {
        let (current, _env) = evidence_env(py, case.root());
        let campaign = f::campaign(py, case.root(), "corpus", &[]);
        f::receipt(
            case.root(),
            "run",
            &json!({"campaign_id":"corpus","status":"PASS",
            "runner_components_sha256":current,"source_sha256":{"source.py":"b".repeat(64)}}),
        );
        let loaded = PyTuple::new(
            py,
            [
                f::json_to_py(py, &json!({"receipt_directories":["receipts"]})),
                PyList::new(py, [&campaign]).unwrap().into_any(),
                PyList::empty(py).into_any(),
            ],
        )
        .unwrap()
        .into_any();
        let _load = f::patch_constant(py, "load_registered_campaigns", &loaded);
        let patches = json!({"status":"CLEAN","repo_root":case.root().to_str().unwrap(),
            "campaigns":1,"stale":[],"unloadable":[]});
        let _patches = f::patch_constant(py, "audit_patches", &f::json_to_py(py, &patches));
        let _root = patch_corpus_root(py, case.root());
        let args = [
            "--baseline".to_owned(),
            case.root()
                .join("baseline.json")
                .to_str()
                .unwrap()
                .to_owned(),
            "--summary".to_owned(),
        ];
        let (rc, report) = f::main(py, &args);
        assert_eq!(rc, 7);
        assert_eq!(report["status"], "REGRESSED");
        assert_eq!(
            report["reproducibility"]["baseline"]["new_uncovered_campaigns"],
            json!(["corpus"])
        );
        assert_eq!(
            repro(py, case.root(), &[campaign])["evidence"][0]["reason"],
            "NO_ACCEPTABLE_RECEIPT"
        );
    });
}

#[test]
fn a_new_file_beside_a_covered_file_is_reported_uncovered() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let covered = f::replace(
            py,
            &f::campaign(py, case.root(), "covered", &[]),
            &json!({"source_sha256":{"src/one.py":"d".repeat(64)},
                "test_sha256":{"tests/test_one.py":"e".repeat(64)}}),
        );
        let changed = [
            "src/one.py",
            "tests/test_one.py",
            "src/two.py",
            "src/nested/three.py",
            "docs/readme.md",
            "native/other/lib.rs",
        ];
        touch(case.root(), &changed);
        let rows = changed_rows(py, case.root(), &changed, std::slice::from_ref(&covered));
        let files: Vec<_> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["file"].as_str().unwrap())
            .collect();
        assert_eq!(files, ["src/nested/three.py", "src/two.py"]);
        assert!(rows
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["reason"] == "NO_PINNING_CAMPAIGN"));
        assert!(rows[0]["detail"].as_str().unwrap().contains("plan one"));
        assert_eq!(changed_rows(py, case.root(), &[], &[covered]), json!([]));
    });
}

#[test]
fn a_deleted_file_inside_territory_is_not_reported_uncovered() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let covered = f::replace(
            py,
            &f::campaign(py, case.root(), "covered", &[]),
            &json!({"source_sha256":{"src/one.py":"d".repeat(64)},"test_sha256":{}}),
        );
        touch(case.root(), &["src/kept.py"]);
        let rows = changed_rows(
            py,
            case.root(),
            &["src/kept.py", "src/deleted.py", "src/gone/deep.py"],
            &[covered],
        );
        assert_eq!(
            rows.as_array()
                .unwrap()
                .iter()
                .map(|r| &r["file"])
                .collect::<Vec<_>>(),
            vec![&json!("src/kept.py")]
        );
    });
}

#[test]
fn measured_territory_is_limited_to_the_campaign_language() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let python = f::replace(
            py,
            &f::campaign(py, case.root(), "python", &[]),
            &json!({"source_sha256":{"src/a.py":"d".repeat(64)}}),
        );
        let mixed = f::replace(
            py,
            &f::campaign(py, case.root(), "mixed", &[]),
            &json!({"language":"python-rust","source_sha256":{"crate/lib.rs":"d".repeat(64)}}),
        );
        let cpp = f::replace(
            py,
            &f::campaign(py, case.root(), "cpp", &[]),
            &json!({"language":"cpp","source_sha256":{"kern/k.cpp":"d".repeat(64)}}),
        );
        let unknown = f::replace(
            py,
            &f::campaign(py, case.root(), "unknown", &[]),
            &json!({"language":"zig","source_sha256":{"z/main.zig":"d".repeat(64)}}),
        );
        let changed = [
            "src/CLAUDE.md",
            "src/data.json",
            "src/b.py",
            "src/deep/c.py",
            "src/deep/notes.md",
            "crate/glue.py",
            "crate/sub/mod.rs",
            "crate/README.md",
            "kern/k.h",
            "kern/sub/x.cc",
            "kern/sub/x.py",
            "z/build.txt",
            "z/sub/other.zig",
        ];
        touch(case.root(), &changed);
        let rows = changed_rows(py, case.root(), &changed, &[python, mixed, cpp, unknown]);
        let files: Vec<_> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["file"].as_str().unwrap())
            .collect();
        assert_eq!(
            files,
            [
                "crate/glue.py",
                "crate/sub/mod.rs",
                "kern/k.h",
                "kern/sub/x.cc",
                "src/b.py",
                "src/deep/c.py",
                "z/build.txt"
            ]
        );
    });
}

#[test]
fn unpinned_changed_files_fail_the_whole_audit() {
    let case = f::isolated_case();
    f::git_repo(case.root());
    Python::attach(|py| {
        let (current, _env) = evidence_env(py, case.root());
        let covered = f::replace(
            py,
            &f::campaign(py, case.root(), "covered", &[]),
            &json!({"source_sha256":{"source.py":"d".repeat(64)},"test_sha256":{}}),
        );
        f::receipt(
            case.root(),
            "covered",
            &json!({"campaign_id":"covered", "status":"PASS",
            "runner_components_sha256":current,
            "source_sha256":{"source.py":f::sha256(py, &case.root().join("source.py"))}}),
        );
        let loaded = PyTuple::new(
            py,
            [
                f::json_to_py(py, &json!({"receipt_directories":["receipts"]})),
                PyList::new(py, [&covered]).unwrap().into_any(),
                PyList::empty(py).into_any(),
            ],
        )
        .unwrap()
        .into_any();
        let _load = f::patch_constant(py, "load_registered_campaigns", &loaded);
        let patches = json!({"status":"CLEAN","repo_root":case.root().to_str().unwrap(),
            "campaigns":1,"stale":[],"unloadable":[]});
        let _patches = f::patch_constant(py, "audit_patches", &f::json_to_py(py, &patches));
        let _root = patch_corpus_root(py, case.root());
        fs::write(case.root().join("sibling.py"), "x = 1\n").unwrap();
        let baseline = f::baseline(
            case.root(),
            &BTreeMap::from([("campaigns_without_value_analysis", vec!["covered"])]),
        );
        let args = [
            "--baseline".to_owned(),
            baseline.to_str().unwrap().to_owned(),
            "--summary".to_owned(),
            "--changed-file".to_owned(),
            "source.py".to_owned(),
            "--changed-file".to_owned(),
            "sibling.py".to_owned(),
        ];
        let (rc, report) = f::main(py, &args);
        assert_eq!(rc, 7);
        let delta = &report["reproducibility"]["baseline"];
        assert_eq!(delta["new_uncovered_changed_files"], json!(["sibling.py"]));
        assert_eq!(delta["status"], "REGRESSED");
        assert_eq!(report["reproducibility"]["uncovered_changed_files"], 1);
    });
}

#[test]
fn value_verdicts_read_slim_receipts_the_same_as_legacy() {
    let case = f::isolated_case();
    Python::attach(|py| {
        let current = f::current();
        let measured = f::measured(py, &f::campaign(py, case.root(), "measured", &[]));
        let value = json!({"tests":[
            {"nodeid":"t.py::test_kills","classification":"CORE"},
            {"nodeid":"t.py::test_inert","classification":"DELETE_CANDIDATE"}
        ]});
        let slim = module(py, "conductor.mutation_receipt_slim")
            .getattr("slim_receipt")
            .unwrap();
        for (stamp, count) in [("20260905T000000Z", 5), ("20260908T000000Z", 80)] {
            let mut payload = f::value_receipt(&current, value.clone(), stamp);
            payload["mutants"] = Value::Array(
                (0..count)
                    .map(|i| json!({"id":format!("m{i}"),"outcome":"KILLED"}))
                    .collect(),
            );
            let compact = f::py_to_json(&slim.call1((f::json_to_py(py, &payload),)).unwrap());
            assert!(compact.get("test_value").is_none());
            let (unmeasured, inert) = f::value_verdicts(
                py,
                case.root(),
                std::slice::from_ref(&measured),
                &json!({"measured":[compact]}),
                &current,
            );
            assert_eq!(unmeasured, json!([]));
            assert_eq!(
                inert
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| &r["nodeid"])
                    .collect::<Vec<_>>(),
                vec![&json!("t.py::test_inert")]
            );
            assert_eq!(inert[0]["reason"], "KILLS_NOTHING");
        }
    });
}
