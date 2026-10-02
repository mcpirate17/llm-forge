#![cfg(feature = "python-compat-tests")]
//! Python snapshot I/O forwards numerical receipts and current source hashes.
#[path = "python_contracts/candidate_review_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
use conductor_native::performance_receipt::{digest, summarize, Identity, Receipt, Sample, SCHEMA};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use support::{module, path};

fn receipt(root: &Path) -> Receipt {
    let samples = vec![
        Sample {
            wall_ms: 2.0,
            cpu_ms: 1.0,
            max_rss_bytes: 4096
        };
        5
    ];
    let mut result = Receipt {
        schema: SCHEMA.into(),
        created_unix: 1,
        identity: Identity {
            host: root.canonicalize().unwrap().display().to_string(),
            head: "head".into(),
            source_files: BTreeMap::from([(
                "core/model.py".into(),
                digest(b"def f(): return 1\n"),
            )]),
            command: "probe".into(),
            workload_sha256: digest(b"probe"),
            environment_sha256: digest(b"env"),
            platform: "cpu".into(),
            toolchain: "rustc".into(),
            warmups: 1,
            timeout_seconds: 2,
        },
        summary: summarize(&samples).unwrap(),
        samples,
        receipt_sha256: String::new(),
    };
    result.seal().unwrap();
    result
}

fn context<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    let namespace = module(py, "types").getattr("SimpleNamespace").unwrap();
    let policy = PyDict::new(py);
    policy.set_item("hot_path_globs", ("core/*",)).unwrap();
    let policy = namespace.call((), Some(&policy)).unwrap();
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo", path(py, root)).unwrap();
    kwargs.set_item("snapshot", path(py, root)).unwrap();
    kwargs.set_item("policy", policy).unwrap();
    kwargs
        .set_item(
            "live_changes",
            PyTuple::new(
                py,
                [
                    fixture::added_change(py, "core/model.py", &["python", "source"]),
                    fixture::added_change(py, "bench/current.json", &["config"]),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    namespace.call((), Some(&kwargs)).unwrap()
}

fn check<'py>(py: Python<'py>, context: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.checks")
        .getattr("check_performance_evidence")
        .unwrap()
        .call1((context, fixture::test_selection(py, &[])))
        .unwrap()
}

#[test]
fn numerical_snapshot_receipt_passes_and_current_source_or_json_drift_fails() {
    let case = fixture::isolated_case();
    fs::create_dir_all(case.root().join("core")).unwrap();
    fs::create_dir_all(case.root().join("bench")).unwrap();
    fs::write(case.root().join("core/model.py"), "def f(): return 1\n").unwrap();
    let current = receipt(case.root());
    fs::write(
        case.root().join("bench/current.json"),
        serde_json::to_vec(
            &serde_json::json!({"schema":"forge.performance-evidence.v1",
            "current":current,"baseline":current,"max_regression_percent":10.0}),
        )
        .unwrap(),
    )
    .unwrap();
    Python::attach(|py| {
        let context = context(py, case.root());
        assert_eq!(
            check(py, &context)
                .getattr("findings")
                .unwrap()
                .len()
                .unwrap(),
            0
        );
        fs::write(case.root().join("core/model.py"), "def f(): return 2\n").unwrap();
        assert_eq!(
            check(py, &context)
                .getattr("findings")
                .unwrap()
                .len()
                .unwrap(),
            1
        );
        fs::write(case.root().join("core/model.py"), "def f(): return 1\n").unwrap();
        fs::write(case.root().join("bench/current.json"), "{malformed").unwrap();
        assert_eq!(
            check(py, &context)
                .getattr("findings")
                .unwrap()
                .len()
                .unwrap(),
            1
        );
        fs::write(
            case.root().join("bench/current.json"),
            "{\"latency\":1,\"benchmark\":true}",
        )
        .unwrap();
        assert_eq!(
            check(py, &context)
                .getattr("findings")
                .unwrap()
                .len()
                .unwrap(),
            1
        );
    });
}
