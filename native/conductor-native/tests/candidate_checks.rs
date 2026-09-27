use conductor_native::candidate_checks::evaluate;
use serde_json::{json, Value};

fn rules(result: &Value) -> Vec<&str> {
    result["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["rule_id"].as_str().unwrap())
        .collect()
}

#[test]
fn integrity_preserves_tree_first_order_and_change_evidence() {
    let request = json!({
        "entries": [
            {"path":"Case.py","mode":"100644","folded":"case.py","repr":"'Case.py'"},
            {"path":"case.py","mode":"100600","folded":"case.py","repr":"'case.py'"},
            {"path":"protected/new.bin","mode":"100644","folded":"protected/new.bin","repr":"'protected/new.bin'"}
        ],
        "changes": [{"path":"protected/new.bin","old_path":"protected/old.bin",
            "status":"A","new_mode":"160000","new_oid":"deadbeef",
            "classes":["binary"],"deleted":false,"exists":true,"size":12}],
        "surface":"ci", "protected_globs":["protected/*"],
        "max_file_bytes":100, "max_binary_bytes":1
    });
    let result = evaluate("candidate-integrity", &request).unwrap();
    assert_eq!(
        rules(&result),
        [
            "case-collision",
            "unsupported-git-mode",
            "submodule-admission",
            "oversized-artifact",
            "binary-admission"
        ]
    );
    assert_eq!(
        result["findings"][0]["message"],
        "case-colliding tree paths are not portable: 'Case.py' and 'case.py'"
    );
    assert_eq!(result["findings"][2]["evidence"]["gitlink_oid"], "deadbeef");
    assert_eq!(
        result["findings"][3]["evidence"],
        json!({"size_bytes":12,"limit_bytes":1})
    );
    assert_eq!(result["metrics"], json!({"tree_entries":3,"changes":1}));
}

#[test]
fn deletion_and_symlink_admission_keep_distinct_attribution() {
    let request = json!({
        "entries": [{"path":"link","mode":"120000","folded":"link","repr":"'link'"}],
        "changes": [
            {"path":"outside/old","old_path":"protected/old","status":"R100",
                "new_mode":"000000","new_oid":"","classes":[],"deleted":true},
            {"path":"link","old_path":null,"status":"A","new_mode":"120000",
                "new_oid":"aaa","classes":[],"deleted":false,"exists":true,"target":"../target"}
        ],
        "surface":"manual", "protected_globs":["protected/*"],
        "max_file_bytes":100, "max_binary_bytes":100
    });
    let result = evaluate("candidate-integrity", &request).unwrap();
    assert_eq!(
        rules(&result),
        ["protected-delete-or-move", "symlink-admission"]
    );
    assert_eq!(result["findings"][0]["path"], "protected/old");
    assert!(result["findings"][0]["evidence"]["destination"].is_null());
    assert_eq!(result["findings"][1]["evidence"]["target"], "../target");
}

#[test]
fn dependency_pairing_and_file_selection_preserve_path_scope() {
    let dep = evaluate(
        "dependency-integrity",
        &json!({
            "paths":["pkg/pyproject.toml","other/Cargo.toml","other/Cargo.lock"],
            "snapshot":"/nonexistent-candidate-snapshot"
        }),
    )
    .unwrap();
    assert_eq!(rules(&dep), ["missing-lockfile"]);
    assert_eq!(
        dep["findings"][0]["message"],
        "dependency manifest has no candidate lockfile: pkg/uv.lock"
    );
    let selected = evaluate(
        "files-for-policy",
        &json!({
            "changes": [
                {"path":"b.py","new_mode":"100644","classes":["python"]},
                {"path":"a.py","new_mode":"100644","classes":["python","test"]},
                {"path":"b.py","new_mode":"100644","classes":["python"]},
                {"path":"gone.py","new_mode":"000000","classes":["python"]}
            ],
            "classes":["python"],"exclude_classes":["test"],"run_on_deletions":false
        }),
    )
    .unwrap();
    assert_eq!(selected["files"], json!(["b.py"]));
}

#[test]
fn dependency_findings_are_stable_across_payload_order() {
    let paths = ["z/pyproject.toml", "a/pyproject.toml", "b/Cargo.toml"];
    let first = evaluate(
        "dependency-integrity",
        &json!({
            "paths": paths, "snapshot": "/nonexistent-candidate-snapshot"
        }),
    )
    .unwrap();
    let second = evaluate(
        "dependency-integrity",
        &json!({
            "paths": paths.into_iter().rev().collect::<Vec<_>>(),
            "snapshot": "/nonexistent-candidate-snapshot"
        }),
    )
    .unwrap();
    assert_eq!(first, second);
    let paths: Vec<_> = first["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        ["a/pyproject.toml", "z/pyproject.toml", "b/Cargo.toml"]
    );
}

#[test]
fn scans_preserve_rule_order_line_numbers_and_messages() {
    let secrets = evaluate(
        "secret-scan",
        &json!({"files":[{"path":"secrets.py","text":
        "token = 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'\nAKIAABCDEFGHIJKLMNOP\n"}]}),
    )
    .unwrap();
    assert_eq!(rules(&secrets), ["aws-access-key", "generic-api-key"]);
    assert_eq!(secrets["findings"][0]["line"], 2);
    assert_eq!(secrets["findings"][1]["line"], 1);
    let native = evaluate(
        "native-source",
        &json!({"files":[{"path":"a.c","text":
        "void f() {\n strcpy(x,y); system(cmd);\n}\n"}]}),
    )
    .unwrap();
    assert_eq!(rules(&native), ["unsafe-native-api", "unsafe-native-api"]);
    assert_eq!(
        native["findings"][0]["message"],
        "unsafe native API admitted: strcpy("
    );
    assert_eq!(native["findings"][0]["line"], 2);
}

#[test]
fn crg_server_test_sentinel_does_not_match_native_secret_patterns() {
    let text = include_str!("../../../src/conductor/test_crg_server.py");
    let scanned = evaluate(
        "secret-scan",
        &json!({
            "files": [{"path": "src/conductor/test_crg_server.py", "text": text}]
        }),
    )
    .unwrap();
    assert_eq!(scanned["findings"], json!([]));
}

#[test]
fn fingerprint_inputs_match_the_previous_python_scan_contract() {
    let scanned = evaluate(
        "secret-scan",
        &json!({"files":[{"path":"cfg/token.txt",
        "text":"first line\nAKIAABCDEFGHIJKLMNOP\n"}]}),
    )
    .unwrap();
    assert_eq!(
        scanned["findings"],
        json!([{
            "check_id":"secret-scan", "rule_id":"aws-access-key", "severity":"critical",
            "message":"candidate contains secret-like credential material",
            "path":"cfg/token.txt", "line":2,
            "help":"Remove and rotate the credential; do not baseline live secrets."
        }])
    );
    let integrity = evaluate(
        "tree-integrity",
        &json!({"entries":[
            {"path":"A.py","mode":"100644","folded":"a.py","repr":"'A.py'"},
            {"path":"a.py","mode":"100600","folded":"a.py","repr":"'a.py'"}
        ]}),
    )
    .unwrap();
    assert_eq!(
        integrity["findings"],
        json!([
            {"check_id":"candidate-integrity", "rule_id":"case-collision",
                "severity":"critical", "path":"a.py",
                "message":"case-colliding tree paths are not portable: 'A.py' and 'a.py'"},
            {"check_id":"candidate-integrity", "rule_id":"unsupported-git-mode",
                "severity":"critical", "path":"a.py",
                "message":"unsupported Git mode 100600 in candidate tree"}
        ])
    );
}

#[test]
fn performance_and_research_decisions_keep_evidence_contract() {
    let chosen = evaluate(
        "performance-selection",
        &json!({
            "changes":[{"path":"core/model.py","classes":["python"]},
                {"path":"core/test_model.py","classes":["python","test"]},
                {"path":"bench/speed.py","classes":["python"]}],
            "hot_globs":["core/*"]
        }),
    )
    .unwrap();
    assert_eq!(chosen["hot_paths"], json!(["core/model.py"]));
    assert_eq!(chosen["evidence_paths"], json!(["bench/speed.py"]));
    let performance = evaluate(
        "performance-evidence",
        &json!({
            "hot_changes":[{"path":"core/model.py","classes":["python"],
                "text":"# performance-critical\ndef f(x): return x"}],
            "evidence_paths":[],"evidence_text":""
        }),
    )
    .unwrap();
    assert_eq!(
        rules(&performance),
        ["missing-performance-budget", "python-only-hotpath"]
    );
    assert_eq!(
        performance["findings"][0]["evidence"]["hot_paths"],
        json!(["core/model.py"])
    );
    let research = evaluate("research-integrity", &json!({
        "changed_text":"score changed; device is cuda", "combined_casefold":"score device baseline",
        "test_text":"", "changed_paths":["research/probe.py"]
    })).unwrap();
    assert_eq!(
        rules(&research),
        [
            "incomplete-result-provenance",
            "missing-numerical-device-tests"
        ]
    );
    assert_eq!(
        research["findings"][0]["message"],
        "research decision path lacks exact identity/provenance fields: seed, config, fingerprint"
    );
}

#[test]
fn python_ast_uses_syntax_and_comment_tokens_and_preserves_order() {
    let source =
        "def f():\n    pass\n# TODO fix\nfor x in xs:\n    for y in ys:\n        eval(y)\n";
    let result = evaluate(
        "python-ast",
        &json!({
            "path":"pkg/m.py", "text":source, "lines":source.lines().collect::<Vec<_>>(),
            "changed_lines":[3], "classes":["python"], "hot":true
        }),
    )
    .unwrap();
    assert_eq!(
        rules(&result),
        [
            "pass-stub",
            "nested-loop-hotpath",
            "dynamic-execution",
            "partial-implementation-marker"
        ]
    );
    assert_eq!(result["findings"][0]["line"], 2);
    assert_eq!(result["findings"][2]["column"], 8);
    assert_eq!(result["findings"][3]["line"], 3);
    let string_only = evaluate("python-ast", &json!({
        "path":"pkg/n.py", "text":"x = '# TODO only string'\n", "lines":["x = '# TODO only string'"],
        "changed_lines":[1], "classes":["python"], "hot":false
    })).unwrap();
    assert!(rules(&string_only).is_empty());
}

#[test]
fn decorated_function_keeps_cpython_finding_order_and_def_location() {
    let mut source = "@wrap(eval('decorator'))\ndef f():\n    exec('body')\n".to_owned();
    source.push_str(&"    value = 1\n".repeat(100));
    let result = evaluate(
        "python-ast",
        &json!({
            "path": "pkg/decorated.py", "text": source,
            "lines": source.lines().collect::<Vec<_>>(),
            "changed_lines": [], "classes": ["python"], "hot": false
        }),
    )
    .unwrap();
    assert_eq!(
        rules(&result),
        [
            "oversized-function",
            "dynamic-execution",
            "dynamic-execution"
        ]
    );
    assert_eq!(result["findings"][0]["line"], 2);
    assert_eq!(result["findings"][0]["column"], 0);
    assert_eq!(
        result["findings"][1]["message"],
        "unsafe dynamic execution via exec"
    );
    assert_eq!(
        result["findings"][2]["message"],
        "unsafe dynamic execution via eval"
    );
}
