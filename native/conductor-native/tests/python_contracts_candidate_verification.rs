#![cfg(feature = "python-compat-tests")]
//! The Python adapter keeps its public Finding and TestSelection shapes.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule, PySet, PyTuple};
use support::{attr_text, module, path, text, AttrPatch, Case};

#[test]
fn malformed_native_request_fails_closed() {
    let _case = Case::new();
    Python::attach(|py| {
        let native = module(py, "conductor._native");
        let decide = native.getattr("candidate_verification_native").unwrap();
        let invalid_json = decide.call1(("selection_plan", "{")).unwrap_err();
        assert!(invalid_json
            .to_string()
            .contains("invalid verification request"));
        let unknown_operation = decide.call1(("unknown", "{}"));
        assert!(unknown_operation
            .unwrap_err()
            .to_string()
            .contains("unknown candidate verification operation"));
    });
}

#[test]
fn definitions_receipt_findings_and_scope_keep_python_shapes() {
    let _case = Case::new();
    Python::attach(|py| {
        let verification = module(py, "conductor.candidate_review.verification");
        let definitions = verification
            .getattr("_python_test_definitions")
            .unwrap()
            .call1((
                "@mark.slow\ndef test_one():\n    assert True\n",
                "test_probe.py",
            ))
            .unwrap();
        assert_eq!(
            text(&definitions.get_item("test_one").unwrap()),
            "@mark.slow\ndef test_one():\n    assert True"
        );
        let payload = module(py, "json").getattr("loads").unwrap()
            .call1((r#"{"missing_evidence":["bad",{"path":"tests/a.py","reason":"no receipt"}],"malformed_receipts":[]}"#,)).unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("waived", Vec::<String>::new()).unwrap();
        let findings = verification
            .getattr("_mutation_receipt_findings")
            .unwrap()
            .call((payload,), Some(&kwargs))
            .unwrap();
        assert_eq!(findings.len().unwrap(), 2);
        assert_eq!(
            attr_text(&findings.get_item(0).unwrap(), "rule_id"),
            "malformed-mutation-receipt"
        );
        let missing = findings.get_item(1).unwrap();
        assert_eq!(attr_text(&missing, "rule_id"), "missing-mutation-receipt");
        assert_eq!(attr_text(&missing, "severity"), "critical");
        assert_eq!(attr_text(&missing, "path"), "tests/a.py");
        let scope = verification
            .getattr("_receipt_required_paths")
            .unwrap()
            .call1((
                vec!["tests/a.py", "tests/b.py"],
                module(py, "json")
                    .getattr("loads")
                    .unwrap()
                    .call1((r#"{"tests/a.py":["tests/a.py::test_new"]}"#,))
                    .unwrap(),
            ))
            .unwrap();
        assert_eq!(scope.extract::<Vec<String>>().unwrap(), vec!["tests/a.py"]);
    });
}

#[test]
fn value_admission_reads_slim_receipt_and_blocks_malformed_envelopes() {
    let case = Case::new();
    Python::attach(|py| {
        let verification = module(py, "conductor.candidate_review.verification");
        let namespace = module(py, "types").getattr("SimpleNamespace").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("snapshot", path(py, case.root())).unwrap();
        let context = namespace.call((), Some(&kwargs)).unwrap();
        let nodeids = module(py, "json")
            .getattr("loads")
            .unwrap()
            .call1((r#"{"t.py":["t.py::test_new"]}"#,))
            .unwrap();
        let malformed = module(py, "json")
            .getattr("loads")
            .unwrap()
            .call1((r#"{"evidence":"not-a-list"}"#,))
            .unwrap();
        let findings = verification
            .getattr("_new_test_value_findings")
            .unwrap()
            .call1((&context, malformed, &nodeids))
            .unwrap();
        assert_eq!(
            attr_text(&findings.get_item(0).unwrap(), "rule_id"),
            "test-value-receipt-unavailable"
        );
        case.write("receipt.json", r#"{"status":"PASS","test_value":{"schema_version":"llm.mutation-testing.test-value.v1","status":"PASS","tests":[{"nodeid":"t.py::test_new","classification":"CORE"}]}}"#);
        let valid = module(py, "json")
            .getattr("loads")
            .unwrap()
            .call1((r#"{"evidence":[{"path":"t.py","receipt":"receipt.json"}]}"#,))
            .unwrap();
        let findings = verification
            .getattr("_new_test_value_findings")
            .unwrap()
            .call1((context, valid, nodeids))
            .unwrap();
        assert_eq!(findings.len().unwrap(), 0);
    });
}

#[test]
fn selection_adapter_preserves_graph_failure_and_structured_finding() {
    let case = Case::new();
    Python::attach(|py| {
        let monkeypatch = module(py, "pytest")
            .getattr("MonkeyPatch")
            .unwrap()
            .call0()
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("inventory", PyDict::new(py)).unwrap();
        let context = module(py, "conductor.test_candidate_review_hardening")
            .getattr("_gate_context")
            .unwrap()
            .call((monkeypatch.clone(), path(py, case.root())), Some(&kwargs))
            .unwrap();
        let model = module(py, "conductor.candidate_review.model");
        let change = model
            .getattr("Change")
            .unwrap()
            .call1((
                "M",
                "conductor/probe.py",
                py.None(),
                "100644",
                "100644",
                "1111111111111111111111111111111111111111",
                "2222222222222222222222222222222222222222",
                vec!["python", "source"],
                "normal",
            ))
            .unwrap();
        let changes = PyTuple::new(py, [change]).unwrap();
        let replace = PyModule::import(py, "dataclasses")
            .unwrap()
            .getattr("replace")
            .unwrap();
        let candidate_kwargs = PyDict::new(py);
        candidate_kwargs.set_item("changes", changes).unwrap();
        let candidate = replace
            .call(
                (&context.getattr("candidate").unwrap(),),
                Some(&candidate_kwargs),
            )
            .unwrap();
        let context_kwargs = PyDict::new(py);
        context_kwargs.set_item("candidate", candidate).unwrap();
        let context = replace.call((&context,), Some(&context_kwargs)).unwrap();
        let verification = module(py, "conductor.candidate_review.verification");
        let mock = module(py, "unittest.mock").getattr("Mock").unwrap();
        let no_tests = PyDict::new(py);
        no_tests
            .set_item("return_value", PySet::empty(py).unwrap())
            .unwrap();
        let empty = mock.call((), Some(&no_tests)).unwrap();
        let _convention = AttrPatch::replace(&verification, "_convention_tests", &empty);
        let no_native = PyDict::new(py);
        no_native.set_item("return_value", PyDict::new(py)).unwrap();
        let empty_native = mock.call((), Some(&no_native)).unwrap();
        let _native = AttrPatch::replace(&verification, "_rust_crate_tests", &empty_native);
        let raised = PyDict::new(py);
        raised
            .set_item(
                "side_effect",
                PyModule::import(py, "builtins")
                    .unwrap()
                    .getattr("RuntimeError")
                    .unwrap()
                    .call1(("stale graph",))
                    .unwrap(),
            )
            .unwrap();
        let failing_graph = mock.call((), Some(&raised)).unwrap();
        let _graph = AttrPatch::replace(&verification, "_graph_test_paths", &failing_graph);
        let selection = verification
            .getattr("select_tests")
            .unwrap()
            .call1((context,))
            .unwrap();
        assert_eq!(selection.getattr("tests").unwrap().len().unwrap(), 0);
        assert_eq!(
            selection
                .getattr("graph")
                .unwrap()
                .get_item("native_test_files")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            0
        );
        let findings = selection.getattr("findings").unwrap();
        assert_eq!(
            attr_text(&findings.get_item(0).unwrap(), "rule_id"),
            "graph-evidence-incomplete"
        );
        assert_eq!(
            attr_text(&findings.get_item(1).unwrap(), "rule_id"),
            "no-targeted-tests"
        );
        monkeypatch.call_method0("undo").unwrap();
    });
}
