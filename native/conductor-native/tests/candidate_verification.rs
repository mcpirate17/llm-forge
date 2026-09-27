#![cfg(feature = "source-analysis")]

use conductor_native::candidate_verification::{decide, python_test_definitions};
use serde_json::json;

#[test]
fn ast_collects_decorated_top_level_and_test_class_definitions_only() {
    let source = "@mark.parametrize('x', [1])\nasync def test_async(x):\n    assert x\n\ndef helper():\n    pass\n\nclass TestShape:\n    @mark.slow\n    def test_method(self):\n        assert True\n\nclass Other:\n    def test_ignored(self):\n        pass\n";
    let definitions = python_test_definitions(source, "test_shape.py").unwrap();
    assert_eq!(definitions.as_object().unwrap().len(), 2);
    assert_eq!(
        definitions["test_async"],
        "@mark.parametrize('x', [1])\nasync def test_async(x):\n    assert x"
    );
    assert_eq!(
        definitions["TestShape::test_method"],
        "    @mark.slow\n    def test_method(self):\n        assert True"
    );
    assert!(python_test_definitions("def broken(:\n", "test_bad.py")
        .unwrap_err()
        .contains("cannot parse test definitions in test_bad.py"));
}

#[test]
fn anchored_inventory_rejects_drift_and_excludes_tombstones() {
    let request = json!({
        "payload": {"schema":"inventory/v1", "anchor_commit":"anchor", "milestone":"w7",
                    "tests":{"tests/test_live.py":["test_a"], "tests/test_dead.py":["test_b"]}},
        "expected_schema":"inventory/v1", "anchor_commit":"anchor", "milestone":"w7",
        "derived":{"tests/test_live.py":["test_a"], "tests/test_dead.py":["test_b"]},
        "present_paths":["tests/test_live.py", "tests/test_dead.py"],
        "dead_paths":["tests/test_dead.py"]
    });
    let effective = decide("inventory_validate", &request).unwrap();
    assert_eq!(effective, json!({"tests/test_live.py":["test_a"]}));
    let mut drift = request.clone();
    drift["derived"]["tests/test_live.py"] = json!(["test_changed"]);
    assert!(decide("inventory_validate", &drift)
        .unwrap_err()
        .contains("label-drift"));
    let mut duplicate = request.clone();
    duplicate["payload"]["tests"]["tests/test_live.py"] = json!(["test_a", "test_a"]);
    assert!(decide("inventory_validate", &duplicate)
        .unwrap_err()
        .contains("duplicate nodeids"));
    let mut unsafe_path = request;
    unsafe_path["payload"]["tests"]["../escape.py"] = json!(["test_escape"]);
    assert!(decide("inventory_validate", &unsafe_path)
        .unwrap_err()
        .contains("path is unsafe"));
}

#[test]
fn value_gate_charges_only_new_or_changed_definitions() {
    let result = decide(
        "gated_nodeids",
        &json!({
            "grandfathered":{"tests/test_existing.py":["test_legacy"]},
            "entries":[
                {"path":"tests/test_existing.py", "classes":["test","python"],
                 "old_mode":"100644", "old_oid":"1", "definitions":{
                     "test_legacy":"same", "test_untouched":"same", "test_changed":"new"},
                 "base":{"test_legacy":"same", "test_untouched":"same", "test_changed":"old"}},
                {"path":"tests/test_new.py", "classes":["test","python"],
                 "old_mode":"000000", "old_oid":"0000000000000000000000000000000000000000",
                 "definitions":{"test_second":"body", "test_first":"body"}, "base":null},
                {"path":"tests/probe.spec.ts", "classes":["test","web"],
                 "old_mode":"000000", "old_oid":"0000000000000000000000000000000000000000"},
                {"path":"tests/test_mutant.patch", "classes":["test"],
                 "old_mode":"000000", "old_oid":"0000000000000000000000000000000000000000"}
            ]
        }),
    )
    .unwrap();
    assert_eq!(
        result["tests/test_existing.py"],
        json!(["tests/test_existing.py::test_changed"])
    );
    assert_eq!(
        result["tests/test_new.py"],
        json!([
            "tests/test_new.py::test_first",
            "tests/test_new.py::test_second"
        ])
    );
    assert_eq!(
        result["tests/probe.spec.ts"],
        json!(["tests/probe.spec.ts"])
    );
    assert!(result.get("tests/test_mutant.patch").is_none());
}

#[test]
fn selection_keeps_native_evidence_out_of_pytest_and_reports_uncovered_python() {
    let plan = decide("selection_plan", &json!({"changes":[
        {"path":"crate/src/lib.rs", "classes":["native"], "risk":"high", "deleted":false},
        {"path":"pkg/widget.py", "classes":["python"], "risk":"normal", "deleted":false},
        {"path":"tests/test_new.py", "classes":["python","test"], "risk":"normal", "deleted":false}
    ]})).unwrap();
    assert_eq!(
        plan["sources"],
        json!(["crate/src/lib.rs", "pkg/widget.py"])
    );
    assert_eq!(plan["changed_tests"], json!(["tests/test_new.py"]));
    let result = decide(
        "selection_decide",
        &json!({
            "sources":plan["sources"], "changed_tests":plan["changed_tests"],
            "high_risk":plan["high_risk"], "graph_tests":[], "convention_tests":[],
            "native_tests":{"crate/src/lib.rs":["crate/src/lib.rs"]},
            "graph":{}, "graph_error":null, "property_evidence":true
        }),
    )
    .unwrap();
    assert_eq!(result["tests"], json!(["tests/test_new.py"]));
    assert_eq!(
        result["evidence_tests"],
        json!(["crate/src/lib.rs", "tests/test_new.py"])
    );
    assert_eq!(result["graph"]["native_test_files"], 1);
    assert_eq!(result["findings"], json!([]));
    let uncovered = decide(
        "selection_decide",
        &json!({
            "sources":plan["sources"], "changed_tests":[], "high_risk":false,
            "graph_tests":[], "convention_tests":[],
            "native_tests":{"crate/src/lib.rs":["crate/src/lib.rs"]},
            "graph":{}, "graph_error":null, "property_evidence":false
        }),
    )
    .unwrap();
    assert_eq!(uncovered["findings"][0]["rule_id"], "no-targeted-tests");
    assert_eq!(
        uncovered["findings"][0]["evidence"]["source_paths"],
        json!(["pkg/widget.py"])
    );
}

#[test]
fn evidence_rows_fail_closed_without_losing_order_or_metrics() {
    let findings = decide(
        "receipt_findings",
        &json!({
            "payload":{"missing_evidence":["bad",{"path":"tests/a.py","reason":"no receipt"}],
                       "malformed_receipts":["truncated"]}, "waived":[]
        }),
    )
    .unwrap();
    assert_eq!(findings[0]["rule_id"], "malformed-mutation-receipt");
    assert_eq!(findings[1]["rule_id"], "missing-mutation-receipt");
    assert_eq!(
        findings[2]["message"],
        "malformed mutation receipt: truncated"
    );
    let plan = decide(
        "value_admission_plan",
        &json!({
            "payload":{"evidence":[{"path":"tests/a.py","receipt":"a.json"},
                                    {"path":"tests/a.py","receipt":"b.json"}, 42]},
            "new_nodeids":{"tests/a.py":["tests/a.py::test_a"],
                           "tests/b.py":["tests/b.py::test_b"]}
        }),
    )
    .unwrap();
    assert_eq!(
        plan["prefix_findings"][0]["rule_id"],
        "duplicate-evidence-row"
    );
    assert_eq!(
        plan["prefix_findings"][1]["rule_id"],
        "malformed-evidence-row"
    );
    assert_eq!(plan["steps"][0]["task"]["receipt"], "a.json");
    assert_eq!(
        plan["steps"][1]["finding"]["rule_id"],
        "new-test-value-not-admitted"
    );
    let required = decide(
        "receipt_required_paths",
        &json!({
            "test_paths":["tests/a.py","tests/b.py"], "gated_nodeids":{"tests/a.py":["nodeid"]}
        }),
    )
    .unwrap();
    assert_eq!(required, json!(["tests/a.py"]));
    let metrics = decide("mutation_metrics", &json!({
        "payload":{"evidence":null,"missing_evidence":{}}, "new_nodeids":{},
        "waiver_states":[{"path":"tests/b.py","active":true}, {"path":"tests/a.py","active":false}]
    })).unwrap();
    assert_eq!(metrics["covered_tests"], 0);
    assert_eq!(metrics["missing_tests"], 0);
    assert_eq!(metrics["mutation_waiver_applied"], json!(["tests/b.py"]));
}

#[test]
fn malformed_receipt_row_uses_python_repr_for_controls_quotes_and_unicode() {
    let findings = decide(
        "receipt_findings",
        &json!({
            "payload": {"missing_evidence": [
                "line\nreturn\r\ttab\u{0000}\u{001f}\u{007f}\u{0085}",
                "it's", "say \"hi\"", "both'\"", "café 😀 中",
                "e\u{0301}", "\u{00a0}\u{200b}\u{2028}",
                "\u{e000}\u{f0000}"
            ]},
            "waived": []
        }),
    )
    .unwrap();
    let rows = findings.as_array().unwrap();
    let prefix = "malformed mutation receipt row (not an object): ";
    let expected = [
        "'line\\nreturn\\r\\ttab\\x00\\x1f\\x7f\\x85'",
        "\"it's\"",
        "'say \"hi\"'",
        "'both\\'\"'",
        "'café 😀 中'",
        "'e\u{0301}'",
        "'\\xa0\\u200b\\u2028'",
        "'\\ue000\\U000f0000'",
    ];
    assert_eq!(rows.len(), expected.len());
    for (row, expected_repr) in rows.iter().zip(expected) {
        assert_eq!(row["message"], format!("{prefix}{expected_repr}"));
        assert!(!row["message"].as_str().unwrap().contains('\n'));
        assert!(!row["message"].as_str().unwrap().contains('\0'));
    }
}

#[test]
fn malformed_evidence_row_recurses_through_nested_values_and_map_keys() {
    let indexed = decide(
        "evidence_index",
        &json!({"evidence_rows": [
            {"bad\n": ["x\u{0000}", {"a'": "😀"}, null, true, 23]}
        ]}),
    )
    .unwrap();
    assert_eq!(indexed["index"], json!({}));
    assert_eq!(indexed["findings"].as_array().unwrap().len(), 1);
    assert_eq!(
        indexed["findings"][0]["message"],
        "evidence row 1 is malformed (needs an object with a string 'path'); \
         the row cannot be evaluated: {'bad\\n': ['x\\x00', {\"a'\": '😀'}, None, True, 23]}"
    );
}

#[test]
fn waiver_decision_preserves_base_file_and_source_failure_reasons() {
    let states = decide("waiver_states", &json!({
        "base":"integration", "waivers":[
            {"id":"off-base", "path":"a.py", "integration_base":"other", "file_ok":true},
            {"id":"drifted", "path":"b.py", "integration_base":"integration", "file_ok":false},
            {"id":"source-drift", "path":"c.py", "integration_base":"integration", "file_ok":true,
             "sources":[{"path":"dep.py","ok":false}]},
            {"id":"active", "path":"d.py", "integration_base":"integration", "file_ok":true,
             "sources":[{"path":"dep.py","ok":true}]}
        ]
    })).unwrap();
    assert_eq!(
        states[0]["reason"],
        "candidate base commit is not the pinned integration base"
    );
    assert_eq!(
        states[1]["reason"],
        "test file missing or drifted from pinned sha256"
    );
    assert_eq!(
        states[2]["reason"],
        "pinned source dep.py missing or drifted from pinned sha256"
    );
    assert_eq!(states[3]["active"], true);
}
