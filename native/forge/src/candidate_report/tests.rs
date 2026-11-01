use super::*;
use serde_json::{json, Value};

fn payload() -> Value {
    json!({"schema_version":1,"receipt_id":"","receipt_digest":"","decision":"pass",
        "surface":"manual","profile":"fast","candidate":{"tree_oid":"0123456789abcdef0123456789abcdef01234567"},
        "policy":{"block_at":"medium"},"timings":{"duration_ms":1234},"cache":{"hits":2},
        "checks":[],"findings":[],"evidence":{"unicode":"雪é","integer":9007199254740993u64,
            "fraction":1.25,"negative_zero":-0.0}})
}
fn finding(rule: &str, severity: &str) -> Value {
    json!({"check_id":"scan","rule_id":rule,"severity":severity,"message":"雪<&>\"'",
        "path":"src/雪.rs","line":2,"column":3,"fingerprint":"abc"})
}
fn check(status: &str) -> Value {
    json!({"check_id":"scan","status":status,"duration_ms":234,"findings":[],
        "stdout_tail":"out<&>","stderr_tail":"err\"'"})
}
fn receipt(p: &Value) -> crate::candidate_receipt::VerifiedReceipt {
    crate::candidate_receipt::seal(&serde_json::to_vec(p).unwrap()).unwrap()
}
fn text(p: &Value, format: Format) -> String {
    String::from_utf8(render(&receipt(p), format).unwrap()).unwrap()
}

#[test]
fn json_retains_unicode_numbers_and_verified_digest_values() {
    let r = receipt(&payload());
    let bytes = render(&r, Format::Json).unwrap();
    let decoded: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(&decoded, r.payload());
    assert_eq!(
        decoded["evidence"]["integer"].as_u64(),
        Some(9007199254740993)
    );
    assert!(crate::candidate_receipt::parse_and_verify(&bytes).is_ok());
    assert_eq!(bytes.last(), Some(&b'\n'));
}

#[test]
fn human_uses_policy_threshold_and_disjoint_suppression_counts() {
    let mut p = payload();
    let mut excepted = finding("excepted", "critical");
    excepted["exception_id"] = json!("EX-1");
    let mut inherited = finding("old", "high");
    inherited["inherited"] = json!(true);
    p["findings"] = json!([
        finding("new", "medium"),
        finding("low", "low"),
        finding(model::WAIVED, "critical"),
        excepted,
        inherited
    ]);
    let output = text(&p, Format::Human);
    assert!(output.contains("recorded decision=pass"));
    assert!(output.contains("execution=unverified"));
    assert!(output.contains("blocking=1 advisory=1 waived=1 excepted=1 inherited=1"));
    assert!(output.find("new-test-value-waived").unwrap() < output.find("scan/new ").unwrap());
    assert!(output.contains("[excepted:EX-1]"));
    assert!(output.contains("[inherited]"));
}

#[test]
fn human_neutralizes_all_dynamic_terminal_lines_and_preserves_all_waivers() {
    let mut p = payload();
    p["findings"] = Value::Array(
        (0..25)
            .map(|_| finding(model::WAIVED, "info"))
            .chain((0..30).map(|_| finding("normal", "info")))
            .collect(),
    );
    p["findings"][0]["message"] = json!("manual\u{1b}[2J\nFORGED\r\u{202e}");
    let output = text(&p, Format::Human);
    assert!(!output.contains('\u{1b}'));
    assert!(!output.contains('\r'));
    assert!(!output.contains('\u{202e}'));
    assert!(output.contains("\\u{a}FORGED\\u{d}"));
    assert_eq!(output.matches("[waived]").count(), 25);
    assert_eq!(output.matches("scan/normal ").count(), 20);
    assert!(output.contains("10 more findings"));
}

#[test]
fn failure_receipt_empty_policy_uses_high_and_missing_inherited_defaults_false() {
    let mut p = payload();
    p["policy"] = json!({});
    p["decision"] = json!("fail");
    p["candidate"] = json!({"kind":"index","tree_oid":"unresolved"});
    p["findings"] = json!([
        finding("review-incomplete", "critical"),
        finding("note", "medium")
    ]);
    let output = text(&p, Format::Human);
    assert!(output.contains("blocking=1 advisory=1"));
    assert!(text(&p, Format::Junit).contains("tests=\"1\" failures=\"1\""));
}

#[test]
fn sarif_preserves_order_locations_rules_and_suppression_evidence() {
    let mut p = payload();
    let mut inherited = finding("same", "high");
    inherited["inherited"] = json!(true);
    let mut excepted = finding("other", "medium");
    excepted["exception_id"] = json!("EX");
    p["findings"] = json!([
        finding("same", "low"),
        inherited,
        excepted,
        finding(model::WAIVED, "info")
    ]);
    let out: Value = serde_json::from_str(&text(&p, Format::Sarif)).unwrap();
    let run = &out["runs"][0];
    assert!(run.get("columnKind").is_none());
    assert_eq!(run["tool"]["driver"]["rules"].as_array().unwrap().len(), 3);
    assert_eq!(run["tool"]["driver"]["rules"][0]["id"], "scan/same");
    let results = run["results"].as_array().unwrap();
    assert_eq!(results.len(), 4);
    assert_eq!(
        results[0]["locations"][0]["physicalLocation"]["region"],
        json!({"startLine":2})
    );
    assert_eq!(results[0]["properties"]["recordedColumn"], 3);
    assert!(results[0].get("suppressions").is_none());
    assert_eq!(results[1]["properties"]["recordedInherited"], true);
    for result in &results[1..] {
        assert!(result.get("suppressions").is_none());
        assert!(result.get("baselineState").is_none());
        assert_eq!(result["properties"]["recordedBlocking"], false);
    }
}

#[test]
fn junit_escapes_attributes_text_and_retains_status_not_recorded_decision() {
    let mut p = payload();
    let mut failed = check("failed");
    failed["findings"] = json!([finding("bad", "high")]);
    let mut skipped = check("skipped");
    skipped["skipped_reason"] = json!("not <selected>\nnext");
    p["checks"] = json!([
        check("passed"),
        failed,
        check("error"),
        skipped,
        check("cached")
    ]);
    let output = text(&p, Format::Junit);
    assert!(output.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    assert!(output.contains("tests=\"5\" failures=\"2\" skipped=\"1\" time=\"1.234\""));
    assert_eq!(output.matches("<failure ").count(), 2);
    assert!(output.contains("雪&lt;&amp;&gt;&quot;&apos;"));
    assert!(output.contains("not &lt;selected&gt;&#10;next"));
    assert!(output.contains("name=\"recorded_decision\" value=\"pass\""));
    assert!(output.contains("name=\"execution_verified\" value=\"false\""));
}

#[test]
fn xml_invalid_controls_fail_instead_of_silently_dropping_evidence() {
    let mut p = payload();
    let mut c = check("passed");
    c["stdout_tail"] = json!("nul\u{0}");
    p["checks"] = json!([c]);
    assert!(render(&receipt(&p), Format::Junit)
        .unwrap_err()
        .to_string()
        .contains("XML-invalid"));
    assert!(render(&receipt(&p), Format::Json).is_ok());
}

#[test]
fn malformed_render_models_fail_for_every_format() {
    let mut invalid = Vec::new();
    for (key, value) in [
        ("schema_version", json!(2)),
        ("decision", json!("approved")),
        ("timings", json!({"duration_ms":-1})),
        ("checks", json!([check("unknown")])),
        ("findings", json!([finding("rule", "fatal")])),
    ] {
        let mut p = payload();
        p[key] = value;
        invalid.push(p);
    }
    let mut p = payload();
    p["policy"] = json!({"block_at":"unknown"});
    invalid.push(p);
    let mut p = payload();
    let mut f = finding("line", "high");
    f["line"] = json!(0);
    p["findings"] = json!([f]);
    invalid.push(p);
    for p in invalid {
        let r = receipt(&p);
        for format in [Format::Human, Format::Json, Format::Sarif, Format::Junit] {
            assert!(render(&r, format).is_err(), "{format:?}");
        }
    }
}

#[test]
fn encoded_amplification_is_bounded_at_writer_not_after_rendering() {
    let mut p = payload();
    let mut c = check("passed");
    c["stdout_tail"] = json!("&".repeat(4 * 1024 * 1024));
    p["checks"] = json!([c]);
    let r = receipt(&p);
    assert!(render(&r, Format::Json).unwrap().len() < MAX_BYTES);
    assert!(render(&r, Format::Junit)
        .unwrap_err()
        .to_string()
        .contains("16 MiB"));
    let mut out = Output::default();
    out.write_all(&vec![b'x'; MAX_BYTES]).unwrap();
    assert!(out.write_all(b"x").is_err());
    assert_eq!(out.bytes.len(), MAX_BYTES);
}

#[test]
fn human_shows_late_blocking_before_twenty_advisories() {
    let mut p = payload();
    let mut findings: Vec<Value> = (0..25).map(|_| finding("advisory", "low")).collect();
    findings.push(finding("late-critical", "critical"));
    p["findings"] = json!(findings);
    let output = text(&p, Format::Human);
    assert!(output.find("scan/late-critical ").unwrap() < output.find("scan/advisory ").unwrap());
    assert_eq!(output.matches("scan/advisory ").count(), 19);
    assert!(output.contains("6 more findings"));
}

#[test]
fn sarif_uri_encoding_and_pathless_raw_columns() {
    let mut p = payload();
    let mut f = finding("uri", "low");
    f["path"] = json!("src/a b#%雪\n.rs");
    let mut pathless = finding("pathless", "low");
    pathless["path"] = Value::Null;
    p["findings"] = json!([f, pathless]);
    let sarif: Value = serde_json::from_str(&text(&p, Format::Sarif)).unwrap();
    let rows = &sarif["runs"][0]["results"];
    assert_eq!(
        rows[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "src/a%20b%23%25%E9%9B%AA%0A.rs"
    );
    assert!(rows[1].get("locations").is_none());
    assert_eq!(rows[1]["properties"]["recordedColumn"], 3);
}

#[test]
fn labels_tree_and_failure_fallback_are_admitted_explicitly() {
    let mut invalid = Vec::new();
    for (key, value) in [
        ("surface", json!("other")),
        ("profile", json!("quick")),
        ("policy", json!({})),
    ] {
        let mut p = payload();
        p[key] = value;
        invalid.push(p);
    }
    for tree in [
        "unresolved",
        "abc",
        "A123456789abcdef0123456789abcdef01234567",
    ] {
        let mut p = payload();
        p["candidate"]["tree_oid"] = json!(tree);
        invalid.push(p);
    }
    for p in invalid {
        assert!(render(&receipt(&p), Format::Json).is_err());
    }
    let mut p = payload();
    p["candidate"]["tree_oid"] = json!("a".repeat(64));
    assert!(render(&receipt(&p), Format::Json).is_ok());
    p["decision"] = json!("fail");
    p["policy"] = json!({});
    p["candidate"]["tree_oid"] = json!("unresolved");
    assert!(render(&receipt(&p), Format::Human).is_ok());
}

#[test]
fn count_admission_precedes_typed_deserialization() {
    let mut p = payload();
    p["checks"] = json!(vec![Value::Null; 20_001]);
    assert!(render(&receipt(&p), Format::Json)
        .unwrap_err()
        .to_string()
        .contains("count limit"));
    let mut p = payload();
    p["findings"] = json!(vec![Value::Null; 100_001]);
    assert!(render(&receipt(&p), Format::Json)
        .unwrap_err()
        .to_string()
        .contains("count limit"));
}

#[test]
fn forged_sarif_flags_remain_recorded_properties_not_standard_suppressions() {
    let mut p = payload();
    let mut f = finding(model::WAIVED, "critical");
    f["exception_id"] = json!("forged");
    f["inherited"] = json!(true);
    p["findings"] = json!([f]);
    let out: Value = serde_json::from_str(&text(&p, Format::Sarif)).unwrap();
    let row = &out["runs"][0]["results"][0];
    assert_eq!(row["level"], "error");
    assert!(row.get("suppressions").is_none());
    assert!(row.get("baselineState").is_none());
    assert_eq!(row["properties"]["recordedExceptionId"], "forged");
    assert_eq!(row["properties"]["recordedInherited"], true);
    assert_eq!(row["properties"]["recordedWaived"], true);
    assert_eq!(out["runs"][0]["properties"]["executionVerified"], false);
}

#[test]
fn junit_recorded_fail_without_checks_has_one_explicit_failure() {
    let mut p = payload();
    p["decision"] = json!("fail");
    let output = text(&p, Format::Junit);
    assert!(output.contains("tests=\"1\" failures=\"1\""));
    assert_eq!(
        output.matches("name=\"recorded-review-verdict\"").count(),
        1
    );
    assert!(output.contains("Recorded decision is fail"));
    let mut failed = check("failed");
    failed["findings"] = json!([finding("bad", "high")]);
    p["checks"] = json!([failed]);
    let output = text(&p, Format::Junit);
    assert!(output.contains("tests=\"1\" failures=\"1\""));
    assert!(!output.contains("name=\"recorded-review-verdict\""));
}

#[test]
fn junit_cached_passed_skipped_and_unmatched_top_level_blockers_get_sentinel() {
    for status in ["cached", "passed", "skipped"] {
        let mut p = payload();
        let mut c = check(status);
        c["findings"] = json!([finding("hidden", "medium")]);
        p["checks"] = json!([c]);
        let output = text(&p, Format::Junit);
        assert!(output.contains("tests=\"2\" failures=\"1\""));
        assert!(output.contains("Recorded blocking evidence is unrepresented"));
        assert_eq!(output.matches("<failure ").count(), 1);
    }
    let mut p = payload();
    let mut failed = check("failed");
    failed["check_id"] = json!("other");
    p["checks"] = json!([failed]);
    p["findings"] = json!([finding("orphan", "high")]);
    assert!(text(&p, Format::Junit).contains("tests=\"2\" failures=\"2\""));
    p["checks"][0]["check_id"] = json!("scan");
    assert!(text(&p, Format::Junit).contains("tests=\"1\" failures=\"1\""));
}

#[test]
fn junit_sentinel_uses_recorded_threshold_and_annotations_without_authenticating() {
    let mut p = payload();
    p["checks"] = json!([check("passed")]);
    let passed = text(&p, Format::Junit);
    assert!(passed.contains("tests=\"1\" failures=\"0\""));
    assert!(!passed.contains("name=\"recorded-review-verdict\""));
    for annotation in ["exception_id", "inherited", "waived", "below-threshold"] {
        let mut f = finding("rule", "medium");
        match annotation {
            "exception_id" => f["exception_id"] = json!("recorded-not-verified"),
            "inherited" => f["inherited"] = json!(true),
            "waived" => f["rule_id"] = json!(model::WAIVED),
            _ => f["severity"] = json!("low"),
        }
        p["findings"] = json!([f.clone()]);
        p["checks"][0]["findings"] = json!([f]);
        let output = text(&p, Format::Junit);
        assert!(
            output.contains("tests=\"1\" failures=\"0\""),
            "{annotation}"
        );
        assert!(!output.contains("name=\"recorded-review-verdict\""));
    }
    p["findings"] = json!([finding("rule", "medium")]);
    p["checks"][0]["findings"] = json!([]);
    assert!(text(&p, Format::Junit).contains("tests=\"2\" failures=\"1\""));
    p["policy"]["block_at"] = json!("high");
    assert!(text(&p, Format::Junit).contains("tests=\"1\" failures=\"0\""));
}
