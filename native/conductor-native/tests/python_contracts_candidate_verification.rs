#![cfg(feature = "python-compat-tests")]
//! The Python adapter keeps its public Finding and TestSelection shapes.

#[path = "python_contracts/candidate_review_support.rs"]
#[allow(dead_code)]
mod candidate_review_support;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
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
        let (context, _anchor) = candidate_review_support::gate_context(
            py,
            case.root(),
            case.root(),
            &[],
            &"c".repeat(40),
        );
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
        let replace = module(py, "dataclasses").getattr("replace").unwrap();
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
    });
}

#[test]
fn missing_evidence_preserves_malformed_blocking_waiver_and_later_rows() {
    let _case = Case::new();
    Python::attach(|py| {
        let verification = module(py, "conductor.candidate_review.verification");
        let missing = verification.getattr("_missing_evidence_findings").unwrap();
        let json = module(py, "json");
        let loads = json.getattr("loads").unwrap();
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("waived", PySet::empty(py).unwrap())
            .unwrap();
        let malformed_row = loads
            .call1((r#"{"missing_evidence":["not-a-dict"]}"#,))
            .unwrap();
        let findings = missing.call((malformed_row,), Some(&kwargs)).unwrap();
        assert_eq!(findings.len().unwrap(), 1);
        let finding = findings.get_item(0).unwrap();
        assert_eq!(attr_text(&finding, "rule_id"), "malformed-mutation-receipt");
        assert_eq!(attr_text(&finding, "severity"), "critical");

        let bad_container = loads.call1((r#"{"missing_evidence":{}}"#,)).unwrap();
        let findings = missing.call((bad_container,), Some(&kwargs)).unwrap();
        let finding = findings.get_item(0).unwrap();
        assert_eq!(
            attr_text(&finding, "rule_id"),
            "malformed-evidence-container"
        );
        assert_eq!(attr_text(&finding, "severity"), "critical");
        assert!(attr_text(&finding, "message").contains("not a list"));

        let missing_fields = loads
            .call1((r#"{"missing_evidence":[{"receipt_rejections":["stale runner pin"]}]}"#,))
            .unwrap();
        let findings = missing.call((missing_fields,), Some(&kwargs)).unwrap();
        let finding = findings.get_item(0).unwrap();
        assert_eq!(attr_text(&finding, "severity"), "critical");
        assert!(finding.getattr("path").unwrap().is_none());
        assert!(
            attr_text(&finding, "message").contains("current automatic PASS evidence is required")
        );
        assert_eq!(
            finding
                .getattr("evidence")
                .unwrap()
                .get_item("receipt_rejections")
                .unwrap()
                .get_item(0)
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "stale runner pin"
        );

        let row = loads.call1((r#"{"missing_evidence":[{"path":"conductor/example.py","reason":"no PASS receipt"}]}"#,)).unwrap();
        let findings = missing.call((row,), Some(&kwargs)).unwrap();
        let finding = findings.get_item(0).unwrap();
        assert_eq!(attr_text(&finding, "rule_id"), "missing-mutation-receipt");
        assert_eq!(
            attr_text(&finding, "message"),
            "conductor/example.py: no PASS receipt -- current automatic PASS evidence is required"
        );
        let help = attr_text(&finding, "help");
        assert!(help.contains("make mutation-generate MUTATION_GENERATE_ARGS='--only SRC'"));
        assert!(!help.contains("MUTATION_SOURCE"));
        assert!(help.contains("Hand-authored") && help.contains("forbidden"));
        kwargs
            .set_item("waived", PySet::new(py, ["conductor/example.py"]).unwrap())
            .unwrap();
        assert_eq!(missing.call((loads.call1((r#"{"missing_evidence":[{"path":"conductor/example.py","reason":"no receipt"}]}"#,)).unwrap(),), Some(&kwargs)).unwrap().len().unwrap(), 0);

        kwargs
            .set_item("waived", PySet::new(py, ["waived.py"]).unwrap())
            .unwrap();
        let mixed = loads.call1((r#"{"missing_evidence":["malformed",{"path":"waived.py","reason":"old debt"},{"path":"active.py","reason":"needs generated evidence"}]}"#,)).unwrap();
        let findings = missing.call((mixed,), Some(&kwargs)).unwrap();
        assert_eq!(findings.len().unwrap(), 2);
        assert_eq!(
            attr_text(&findings.get_item(0).unwrap(), "rule_id"),
            "malformed-mutation-receipt"
        );
        assert!(findings
            .get_item(0)
            .unwrap()
            .getattr("path")
            .unwrap()
            .is_none());
        assert_eq!(
            attr_text(&findings.get_item(1).unwrap(), "rule_id"),
            "missing-mutation-receipt"
        );
        assert_eq!(
            attr_text(&findings.get_item(1).unwrap(), "path"),
            "active.py"
        );
    });
}

#[test]
fn slim_receipt_detail_still_admits_classified_new_test() {
    let case = Case::new();
    Python::attach(|py| {
        let json = module(py, "json");
        let loads = json.getattr("loads").unwrap();
        let payload = loads.call1((r#"{"campaign_id":"c-slim","status":"PASS","generated_at":"2026-09-13T00:00:00+00:00","test_value":{"schema_version":"llm.mutation-testing.test-value.v1","status":"PASS","tests":[{"nodeid":"t.py::test_new","classification":"CORE"}]}}"#,)).unwrap();
        let mutants = pyo3::types::PyList::empty(py);
        for index in 0..80 {
            let mutant = PyDict::new(py);
            mutant.set_item("id", format!("m{index}")).unwrap();
            mutant.set_item("outcome", "KILLED").unwrap();
            mutants.append(mutant).unwrap();
        }
        payload.set_item("mutants", mutants).unwrap();
        let slim = module(py, "conductor.mutation_receipt_slim")
            .getattr("slim_receipt")
            .unwrap()
            .call1((payload,))
            .unwrap();
        assert!(!slim.contains("test_value").unwrap());
        let encoded = json
            .getattr("dumps")
            .unwrap()
            .call1((&slim,))
            .unwrap()
            .extract::<String>()
            .unwrap();
        case.write("receipt.json", &encoded);
        let evidence = loads
            .call1((
                r#"{"evidence":[{"path":"t.py","receipt":"receipt.json","campaign_id":"c-slim"}]}"#,
            ))
            .unwrap();
        let nodeids = loads.call1((r#"{"t.py":["t.py::test_new"]}"#,)).unwrap();
        let namespace = module(py, "types").getattr("SimpleNamespace").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("snapshot", path(py, case.root())).unwrap();
        let context = namespace.call((), Some(&kwargs)).unwrap();
        let findings = module(py, "conductor.candidate_review.verification")
            .getattr("_new_test_value_findings")
            .unwrap()
            .call1((context, evidence, nodeids))
            .unwrap();
        assert_eq!(findings.len().unwrap(), 0);
    });
}
