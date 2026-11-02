use super::*;
use crate::duplicate_fingerprints::{analyze_file, FingerprintPolicy};

fn function(name: &str, params: &str, doc: &str) -> String {
    format!("def {name}({params}):\n    \"{doc}\"\n    a = 1\n    b = 2\n    c = 3\n    d = 4\n    e = 5\n    f = 6\n    g = 7\n    return a\n")
}
fn record(path: &str, text: String, lines: Value) -> Value {
    json!({"path":path,"text":text,"changed_lines":lines})
}

#[test]
fn candidate_names_parameters_and_docstrings_are_ignored_but_bodies_are_not() {
    let first = function("original", "x", "first");
    let renamed = function("copied", "different=1", "second");
    let payload = json!({"files":[
        record("z.py", first.clone(), json!([])),
        record("a.py", renamed.clone(), json!([10])),
        record("other.py", renamed.replace("g = 7", "g = 8"), json!([]))
    ],"changed":["a.py"]});
    let result = evaluate(&payload).unwrap();
    assert_eq!(result["metrics"]["candidate_function_bodies"], 3);
    assert_eq!(result["files"], json!(["a.py"]));
    assert_eq!(
        result["findings"],
        json!([{
            "check_id":"duplicate-function-bodies","rule_id":"copied-function-body",
            "severity":"high","path":"a.py","line":1,
            "message":"changed function copied duplicates z.py:1 (original)",
            "evidence":{"peers":[["z.py","original",1]]}
        }])
    );
    let a = analyze_file("a.py".into(), first, FingerprintPolicy::Standalone).unwrap();
    let b = analyze_file("b.py".into(), renamed, FingerprintPolicy::Standalone).unwrap();
    assert_ne!(a.functions[0].digest, b.functions[0].digest);
}

#[test]
fn cr_only_functions_keep_ten_line_threshold_and_changed_span() {
    let a = function("a", "", "").replace('\n', "\r");
    let b = function("b", "", "").replace('\n', "\r");
    let mut payload = json!({"files":[record("a.py",a,json!([5])),record("b.py",b,json!([]))],"changed":["a.py"]});
    let result = evaluate(&payload).unwrap();
    assert_eq!(result["metrics"]["candidate_function_bodies"], 2);
    assert_eq!(result["findings"].as_array().unwrap().len(), 1);
    payload["files"][0]["changed_lines"] = json!([20]);
    assert!(evaluate(&payload).unwrap()["findings"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn changed_spans_are_inclusive_and_peer_evidence_is_complete_sorted_and_message_capped() {
    let mut files = (0..7)
        .rev()
        .map(|i| record(&format!("{i}.py"), function("same", "", ""), json!([])))
        .collect::<Vec<_>>();
    files.push(record("target.py", function("target", "", ""), json!([10])));
    let mut payload = json!({"files":files,"changed":["target.py"]});
    let output = evaluate(&payload).unwrap();
    let finding = &output["findings"][0];
    assert_eq!(finding["evidence"]["peers"].as_array().unwrap().len(), 7);
    assert_eq!(finding["evidence"]["peers"][0], json!(["0.py", "same", 1]));
    let message = finding["message"].as_str().unwrap();
    assert!(message.contains("4.py:1 (same)"));
    assert!(!message.contains("5.py"));
    payload["files"][7]["changed_lines"] = json!([11]);
    assert!(evaluate(&payload).unwrap()["findings"]
        .as_array()
        .unwrap()
        .is_empty());
    payload["files"][7]["changed_lines"] = json!([1]);
    assert_eq!(
        evaluate(&payload).unwrap()["findings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn malformed_unchanged_peer_is_incomplete_not_omitted_even_without_changed_files() {
    for changed in [json!([]), json!(["good.py"])] {
        let payload = json!({"files":[
            record("good.py", function("good", "", ""), json!([])),
            record("bad.py", "def broken(:\n".into(), json!([]))
        ],"changed":changed});
        let error = evaluate(&payload).unwrap_err();
        assert!(error.contains("bad.py"));
        assert!(error.contains("incomplete"));
    }
}

#[test]
fn ten_line_threshold_and_breadth_first_function_order_are_preserved() {
    let short = "def short():\n    a = 1\n    b = 2\n    c = 3\n    d = 4\n    e = 5\n    f = 6\n    return a\n";
    let nested = function("inner", "", "")
        .lines()
        .map(|line| format!("    {line}\n"))
        .collect::<String>();
    let source = format!(
        "def outer():\n{nested}\n{}{}",
        function("sibling", "", ""),
        short
    );
    let parsed = analyze_file("a.py".into(), source, FingerprintPolicy::Candidate).unwrap();
    assert_eq!(
        parsed
            .functions
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["outer", "sibling", "inner"]
    );
    assert_eq!(
        analyze_file("a.py".into(), short.into(), FingerprintPolicy::Standalone)
            .unwrap()
            .functions
            .len(),
        1
    );
}

#[test]
fn malformed_payloads_and_inventory_gaps_fail_loudly() {
    let good = record("a.py", function("a", "", ""), json!([]));
    for payload in [
        json!({"files":[good.clone(),good.clone()],"changed":[]}),
        json!({"files":[good.clone()],"changed":["absent.py"]}),
        json!({"files":[record("../escape.py", "".into(), json!([]))],"changed":[]}),
        json!({"files":[record("a.py", "".into(), json!([0]))],"changed":["a.py"]}),
        json!({"files":[record("a.py", "".into(), json!([true]))],"changed":["a.py"]}),
        json!({"files":[record("a.py", "".into(), json!([1]))],"changed":[]}),
        json!({"files":[record("a.py", "x".repeat(MAX_TEXT+1), json!([]))],"changed":[]}),
        json!({"files":vec![good;MAX_FILES+1],"changed":[]}),
    ] {
        assert!(evaluate(&payload).is_err());
    }
}

#[test]
fn node_depth_and_peer_amplification_are_admitted_before_policy_output() {
    let mut remaining = MAX_NODES;
    assert!(admit_source(&"x = 1\n".repeat(40_000), &mut remaining).is_err());
    let mut remaining = MAX_NODES;
    let deep = format!("x = {}1{}\n", "[".repeat(140), "]".repeat(140));
    assert!(admit_source(&deep, &mut remaining).is_err());
    let mut groups = vec![Group {
        locations: (0..4000)
            .map(|n| (format!("{}-{n}.py", "x".repeat(4090)), "same".into(), 1))
            .collect(),
    }];
    let changed = groups[0].locations.iter().take(100).cloned().collect();
    assert!(admit_findings(&mut groups, &changed, &[])
        .unwrap_err()
        .contains("peer evidence"));
    let mut groups = vec![Group {
        locations: (0..MAX_FINDINGS + 2)
            .map(|n| (format!("{n}.py"), "f".into(), 1))
            .collect(),
    }];
    let changed = groups[0].locations.iter().cloned().collect();
    assert!(admit_findings(&mut groups, &changed, &[]).is_err());
}

#[test]
fn source_bound_compatibility_preserves_syntax_flag_and_surfaces_admission_errors() {
    let malformed = analyze_file(
        "bad.py".into(),
        "def bad(:\n".into(),
        FingerprintPolicy::Candidate,
    )
    .unwrap();
    assert!(malformed.parse_error);
    assert!(malformed.functions.is_empty());
    let oversized = analyze_file(
        "large.py".into(),
        "x".repeat(MAX_TEXT + 1),
        FingerprintPolicy::Standalone,
    );
    assert!(matches!(
        oversized,
        Err(crate::candidate_python::Error::Limit(_))
    ));
    let source = "x = 1\n".repeat(10_000);
    // Every file fits the per-document 65536-node cap; the supplied peer
    // inventory as a whole crosses its independent one-million-node budget.
    let files = (0..40)
        .map(|n| record(&format!("{n}.py"), source.clone(), json!([])))
        .collect::<Vec<_>>();
    let output = evaluate(&json!({"files":files,"changed":[]}));
    assert!(output.unwrap_err().contains("aggregate nodes"));
}
