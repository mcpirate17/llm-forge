use conductor_native::candidate_policy::{classify, fragment, parse_policy, parse_value_waivers};
use serde_json::{json, Value};

fn policy() -> Value {
    json!({
        "schema_version": 1, "block_at": "high", "max_workers": 2,
        "cache_ttl_days": 7, "claim_max_age_hours": 24,
        "max_file_bytes": 100000, "max_binary_bytes": 200000,
        "coverage_threshold": 80.0, "high_risk_coverage_threshold": 90.0,
        "baseline_expires": "2026-10-20",
        "classes": {"governance": ["conductor/*"]},
        "risk": {"high": ["conductor/candidate_review/*"]},
        "paths": {"protected_deletes": [], "hot": [], "generated": []},
        "checks": {"candidate-integrity": {
            "kind": "builtin", "profiles": ["fast", "full"], "classes": [],
            "severity": "critical", "always": true
        }}
    })
}

#[test]
fn normalized_schema_and_defaults_are_stable() {
    let parsed = parse_policy(&policy(), "2026-09-27").unwrap();
    assert_eq!(parsed["block_at"], "high");
    assert_eq!(parsed["checks"][0]["attribution"], "candidate");
    assert_eq!(parsed["checks"][0]["timeout_seconds"], 60);
    assert_eq!(parsed["checks"][0]["cache"], true);
    assert_eq!(parsed["checks"][0]["wall_timeout_override"], 0);
    assert_eq!(parsed["checks"][0]["profiles"], json!(["fast", "full"]));
    assert_eq!(parsed["baseline_expires"], "2026-10-20");
}

#[test]
fn parser_preserves_precise_refusals() {
    let mut raw = policy();
    raw["extra"] = json!(true);
    assert_eq!(
        parse_policy(&raw, "2026-09-27").unwrap_err(),
        "candidate policy has unknown top-level keys: ['extra']"
    );
    raw = policy();
    raw["checks"]["candidate-integrity"]["attribution"] = json!("guess");
    assert_eq!(
        parse_policy(&raw, "2026-09-27").unwrap_err(),
        "checks.candidate-integrity.attribution must be one of ['candidate', 'diff']"
    );
    raw = policy();
    raw["baseline_expires"] = json!("2026-09-26");
    assert_eq!(
        parse_policy(&raw, "2026-09-27").unwrap_err(),
        "policy baseline window expired on 2026-09-26; refresh and re-review it"
    );
}

#[test]
fn exception_scope_and_value_waiver_errors_are_exact() {
    assert_eq!(
        fragment("exception_path", &json!("**"), "2026-09-27").unwrap_err(),
        "exception path is a forbidden blanket scope: '**'"
    );
    assert_eq!(parse_value_waivers(&json!([{"integration_base": "abc",
        "nodeids": ["a.py::test_a"], "reason": "reason", "approved_by": "Tim",
        "approved_on": "2026-09-01"}])).unwrap_err(),
        "value_waivers.integration_base must be the full 40-hex commit oid of the integration base it binds to: 'abc'");
}

#[test]
fn classification_includes_both_rename_sides_and_intrinsic_families() {
    let change = json!({"path": "src/pkg/new.py", "old_path": "conductor/candidate_review/old.rs",
        "new_mode": "100644", "old_mode": "120000"});
    let globs = json!({"class_globs": {"governance": ["conductor/*"]},
        "generated_globs": ["src/pkg/*"], "high_risk_globs": ["conductor/candidate_review/*"]});
    assert_eq!(
        classify(&change, &globs).unwrap(),
        json!({
        "classes": ["generated", "governance", "native", "python", "rust", "source", "symlink"],
        "risk": "high"})
    );
}

#[test]
fn primitive_refusals_cover_type_range_and_scope_errors() {
    let bad = [
        (
            "strings",
            json!({"field":"value","value":"bad"}),
            "array of non-empty strings",
        ),
        (
            "strings",
            json!({"field":"value","value":[],"allow_empty":false}),
            "must not be empty",
        ),
        (
            "positive",
            json!({"field":"value","value":false}),
            "positive integer",
        ),
        (
            "positive",
            json!({"field":"value","value":17,"maximum":16}),
            "must be <= 16",
        ),
        (
            "percent",
            json!({"field":"value","value":101}),
            "between 0 and 100",
        ),
        (
            "boolean",
            json!({"field":"value","value":1}),
            "must be a boolean",
        ),
        (
            "date",
            json!({"field":"value","value":"not-a-date"}),
            "must be an ISO date",
        ),
        ("exception_path", json!("*"), "forbidden blanket scope"),
        (
            "exception_path",
            json!("/absolute/path"),
            "forbidden blanket scope",
        ),
        ("exception_path", json!("single*"), "two literal segments"),
        ("baselines", json!([]), "baselines must be a table"),
        (
            "baselines",
            json!({"bad":{"path":"only-one-key"}}),
            "invalid schema",
        ),
    ];
    for (operation, input, expected) in bad {
        let error = fragment(operation, &input, "2026-09-27").unwrap_err();
        assert!(error.contains(expected), "{operation}: {error}");
    }
}

#[test]
fn check_and_exception_schema_refuse_each_invalid_axis() {
    let invalid_checks = [
        (Value::Null, "must be a table"),
        (json!({"kind":"builtin","unknown":1}), "unknown keys"),
        (json!({"kind":"unknown","profiles":["fast"]}), ".kind"),
        (
            json!({"kind":"builtin","profiles":["unknown"]}),
            "unknown profile",
        ),
        (
            json!({"kind":"builtin","profiles":["fast"],"classes":["unknown"]}),
            "unknown class",
        ),
        (
            json!({"kind":"builtin","profiles":["fast"],"exclude_classes":["unknown"]}),
            "unknown class",
        ),
        (
            json!({"kind":"command","profiles":["fast"]}),
            "requires command and version_command",
        ),
        (
            json!({"kind":"builtin","profiles":["fast"],"severity":"unknown"}),
            ".severity is invalid",
        ),
    ];
    for (raw, expected) in invalid_checks {
        let error = fragment("check", &json!({"id":"bad","value":raw}), "2026-09-27").unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
    let invalid_exceptions = [
        (Value::Null, "must be a table"),
        (json!({"unknown":true}), "unknown keys"),
        (
            json!({"id":"missing-required-fields"}),
            "missing required keys",
        ),
        (
            json!({"id":"short","check":"python-ast","path":"conductor/probe.py",
                "owner":"x","justification":"too short","expires":"2026-09-27"}),
            "not specific enough",
        ),
    ];
    for (raw, expected) in invalid_exceptions {
        let error = fragment("exception", &raw, "2026-09-27").unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn value_waiver_parser_refuses_empty_pattern_and_short_base() {
    let mut waiver = json!({
        "integration_base":"dddddddddddddddddddddddddddddddddddddddd",
        "nodeids":["conductor/test_repo_index.py::test_the_index_is_not_degenerate"],
        "reason":"Bounded test value exception", "approved_by":"Tim",
        "approved_on":"2026-09-02", "expires":"2026-10-02"
    });
    let parsed = parse_value_waivers(&json!([waiver.clone()])).unwrap();
    assert_eq!(parsed[0]["nodeids"], waiver["nodeids"]);
    assert_eq!(parsed[0]["approved_on"], "2026-09-02");
    assert_eq!(parsed[0]["expires"], "2026-10-02");
    waiver["nodeids"] = json!([]);
    assert!(parse_value_waivers(&json!([waiver.clone()]))
        .unwrap_err()
        .contains("non-empty"));
    waiver["nodeids"] = json!(["conductor/test_repo_index.py::test_*"]);
    assert!(parse_value_waivers(&json!([waiver.clone()]))
        .unwrap_err()
        .contains("exact, never patterns"));
    waiver["nodeids"] = json!(["conductor/test_repo_index.py::test_the_index_is_not_degenerate"]);
    waiver["integration_base"] = json!("dddddddddddd");
    assert!(parse_value_waivers(&json!([waiver]))
        .unwrap_err()
        .contains("full 40-hex"));
}

#[test]
fn baseline_and_exception_dates_are_bounded_and_checks_are_known() {
    let today = "2026-09-27";
    let mut raw = policy();
    raw["exceptions"] = json!([{
        "id":"bounded", "check":"candidate-integrity", "path":"conductor/probe.py",
        "owner":"grok", "justification":"Specific temporary policy exception",
        "expires":"2026-10-01"
    }]);
    assert_eq!(
        parse_policy(&raw, today).unwrap()["exceptions"][0]["exception_id"],
        "bounded"
    );
    raw["exceptions"][0]["check"] = json!("unknown");
    assert!(parse_policy(&raw, today)
        .unwrap_err()
        .contains("names an unknown check"));
    raw["exceptions"][0]["check"] = json!("candidate-integrity");
    raw["exceptions"][0]["expires"] = json!("2026-09-26");
    let parsed = parse_policy(&raw, today).unwrap();
    assert_eq!(parsed["exceptions"].as_array().unwrap().len(), 0);
    assert_eq!(parsed["expired_exceptions"][0]["exception_id"], "bounded");
    raw["exceptions"][0]["expires"] = json!("2026-09-27");
    let parsed = parse_policy(&raw, today).unwrap();
    assert_eq!(parsed["exceptions"][0]["exception_id"], "bounded");
    assert_eq!(parsed["expired_exceptions"].as_array().unwrap().len(), 0);
    raw["exceptions"][0]["expires"] = json!("2026-12-27");
    assert!(parse_policy(&raw, today)
        .unwrap_err()
        .contains("more than 90 days"));
    raw["exceptions"][0]["expires"] = json!("2026-10-01");
    let duplicate = raw["exceptions"][0].clone();
    raw["exceptions"].as_array_mut().unwrap().push(duplicate);
    assert!(parse_policy(&raw, today)
        .unwrap_err()
        .contains("identifiers must be unique"));
}

#[test]
fn classification_covers_shell_web_dependency_and_symlink_paths() {
    let globs = json!({"class_globs":{},"generated_globs":[],"high_risk_globs":[]});
    let cases = [
        ("script.sh", "100755", &["shell", "source"][..]),
        ("ui.ts", "100644", &["web", "source"][..]),
        (
            "pyproject.toml",
            "100644",
            &["toml", "python_dependency"][..],
        ),
        ("Cargo.lock", "100644", &["rust_dependency"][..]),
        ("link", "120000", &["symlink"][..]),
    ];
    for (path, mode, expected) in cases {
        let classes = classify(&json!({"path":path,"new_mode":mode}), &globs).unwrap();
        let actual = classes["classes"].as_array().unwrap();
        for class in expected {
            assert!(actual.contains(&json!(class)), "{path} missing {class}");
        }
    }
}
