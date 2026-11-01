//! Integration contracts for assessment of supplied results, not execution.
#![cfg(target_os = "linux")]
use forge::candidate_assessment::{assess, CheckResult, Finding};
use forge::candidate_policy::Policy;
use forge::candidate_source::Candidate;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const TODAY: &str = "2026-11-01";
const UNICODE_CANONICAL: &str =
    r#"{"check_id":"lint","line":7,"message":"雪 🦀 café","path":"src/é.rs","rule_id":"unicode"}"#;
// Independently obtained by printf of the literal UTF-8 canonical bytes | sha256sum.
const UNICODE_SHA256: &str = "30cbd9bba8d989c6690c402882b2edaa95afaa9ef4d45ff7fb88f7eeb385a992";
const UNICODE_FINGERPRINT: &str = "30cbd9bba8d989c6690c4028";

fn raw_policy(attribution: &str, block_at: &str) -> Value {
    json!({
        "schema_version":1, "block_at":block_at, "max_workers":2,
        "cache_ttl_days":7, "claim_max_age_hours":24,
        "max_file_bytes":100000, "max_binary_bytes":200000,
        "coverage_threshold":80.0, "high_risk_coverage_threshold":90.0,
        "baseline_expires":"2026-12-01", "classes":{}, "risk":{"high":[]},
        "paths":{"protected_deletes":[],"hot":[],"generated":[]},
        "checks":{
            "lint":{"kind":"builtin","profiles":["fast","full"],"severity":"high","attribution":attribution},
            "other":{"kind":"builtin","profiles":["fast"],"severity":"high"}
        }
    })
}

fn parsed_policy(raw: Value) -> Policy {
    Policy {
        path: "policy.toml".into(),
        digest: "e".repeat(64),
        document: conductor_native::candidate_policy::parse_policy(&raw, TODAY).unwrap(),
    }
}

fn policy(attribution: &str, block_at: &str) -> Policy {
    parsed_policy(raw_policy(attribution, block_at))
}

fn candidate() -> Candidate {
    serde_json::from_value(json!({
        "kind":"index", "tree_oid":"a".repeat(40), "base_tree_oid":"b".repeat(40),
        "base_commit_oid":null, "commit_oid":null, "target_ref":null,
        "integration_base_oid":null, "integration_base_detail":"unresolved fixture",
        "changes":[{
            "status":"R100", "path":"src/new.rs", "old_path":"src/old.rs",
            "old_mode":"100644", "new_mode":"100644", "old_oid":"c".repeat(40),
            "new_oid":"c".repeat(40), "classes":["rust","source"], "risk":"normal"
        },{
            "status":"D", "path":"src/deleted.rs", "old_path":null,
            "old_mode":"100644", "new_mode":"000000", "old_oid":"d".repeat(40),
            "new_oid":"0".repeat(40), "classes":["rust","source"], "risk":"normal"
        }]
    }))
    .unwrap()
}

fn finding(check: &str, path: Option<&str>, message: &str, severity: &str) -> Value {
    json!({
        "check_id":check, "rule_id":"lint-rule", "severity":severity,
        "message":message, "path":path, "line":null, "column":null, "help":null,
        "evidence":{}, "fingerprint":"", "exception_id":null, "inherited":false
    })
}

fn result(check: &str, status: &str, findings: Vec<Value>, files: &[&str]) -> CheckResult {
    serde_json::from_value(json!({
        "check_id":check, "status":status, "duration_ms":1, "findings":findings,
        "files":files, "command":[], "tool_version":null,
        "cache_key":if status=="cached" {Some("fixture-cache")} else {None},
        "cache_hit":status=="cached", "exit_code":null, "skipped_reason":null,
        "stdout_tail":"", "stderr_tail":"", "metrics":{}
    }))
    .unwrap()
}

fn evaluate(policy: &Policy, results: &[CheckResult]) -> Value {
    serde_json::to_value(assess(policy, &candidate(), results).unwrap()).unwrap()
}

fn exception(id: &str, check: &str, path: &str, fingerprint: Option<&str>) -> Value {
    json!({
        "id":id, "check":check, "path":path, "fingerprint":fingerprint,
        "owner":"Tim", "justification":"Specific fixture exception for a known measured finding",
        "expires":"2026-11-02"
    })
}

fn unicode_finding() -> Value {
    let mut value = finding("lint", Some("src/é.rs"), "雪 🦀 café", "high");
    value["rule_id"] = json!("unicode");
    value["line"] = json!(7);
    value
}

#[test]
fn unicode_fingerprint_matches_independent_canonical_bytes_and_digest() {
    assert_eq!(
        format!("{:x}", Sha256::digest(UNICODE_CANONICAL.as_bytes())),
        UNICODE_SHA256
    );
    assert_eq!(&UNICODE_SHA256[..24], UNICODE_FINGERPRINT);
    let assessment = evaluate(
        &policy("candidate", "high"),
        &[result(
            "lint",
            "failed",
            vec![unicode_finding()],
            &["src/é.rs"],
        )],
    );
    assert_eq!(
        assessment["findings"][0]["fingerprint"],
        UNICODE_FINGERPRINT
    );
    assert_eq!(assessment["decision"], "fail");
    assert_eq!(assessment["scope"], "supplied_results");
    assert_eq!(assessment["review_complete"], false);
    assert_eq!(assessment["execution_verified"], false);
}

#[test]
fn fingerprint_excludes_annotations_severity_and_evidence_and_keeps_explicit_override() {
    let mut annotated = unicode_finding();
    annotated["column"] = json!(999);
    annotated["help"] = json!("different remediation");
    annotated["evidence"] = json!({"run":"different","values":[1,2]});
    annotated["severity"] = json!("medium");
    annotated["inherited"] = json!(true);
    annotated["exception_id"] = json!("forged-exception");
    let policy = policy("candidate", "high");
    let assessment = evaluate(
        &policy,
        &[result("lint", "failed", vec![annotated.clone()], &[])],
    );
    assert_eq!(
        assessment["findings"][0]["fingerprint"],
        UNICODE_FINGERPRINT
    );
    assert_eq!(assessment["findings"][0]["inherited"], false);
    assert!(assessment["findings"][0]["exception_id"].is_null());
    annotated["fingerprint"] = json!("0123456789abcdef01234567");
    let assessment = evaluate(&policy, &[result("lint", "failed", vec![annotated], &[])]);
    assert_eq!(
        assessment["findings"][0]["fingerprint"],
        "0123456789abcdef01234567"
    );
}

#[test]
fn every_severity_threshold_is_inclusive_and_uses_the_declared_order() {
    let severities = ["info", "low", "medium", "high", "critical"];
    for (threshold_rank, threshold) in severities.iter().enumerate() {
        let policy = policy("candidate", threshold);
        for (finding_rank, severity) in severities.iter().enumerate() {
            let assessment = evaluate(
                &policy,
                &[result(
                    "lint",
                    "failed",
                    vec![finding("lint", Some("src/new.rs"), "finding", severity)],
                    &[],
                )],
            );
            assert_eq!(
                assessment["decision"],
                if finding_rank >= threshold_rank {
                    "fail"
                } else {
                    "pass"
                },
                "threshold={threshold} severity={severity}"
            );
        }
    }
}

#[test]
fn diff_attribution_counts_new_old_and_deleted_paths_and_recomputes_annotations() {
    let mut findings = Vec::new();
    for (path, message) in [
        (Some("src/new.rs"), "new"),
        (Some("src/old.rs"), "renamed-from"),
        (Some("src/deleted.rs"), "deleted"),
        (Some("src/unrelated.rs"), "debt"),
        (None, "aggregate"),
    ] {
        let mut value = finding("lint", path, message, "high");
        value["inherited"] = json!(path != Some("src/unrelated.rs"));
        findings.push(value);
    }
    let assessment = evaluate(
        &policy("diff", "high"),
        &[result("lint", "failed", findings, &[])],
    );
    assert_eq!(assessment["decision"], "fail");
    let inherited: Vec<_> = assessment["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["inherited"].as_bool().unwrap())
        .collect();
    assert_eq!(inherited, [false, false, false, true, true]);
    let mut spoofed = finding("lint", Some("src/unrelated.rs"), "must still block", "high");
    spoofed["inherited"] = json!(true);
    spoofed["exception_id"] = json!("spoofed");
    let assessment = evaluate(
        &policy("candidate", "high"),
        &[result("lint", "failed", vec![spoofed], &[])],
    );
    assert_eq!(assessment["decision"], "fail");
    assert_eq!(assessment["findings"][0]["inherited"], false);
    assert!(assessment["findings"][0]["exception_id"].is_null());
}

#[test]
fn live_path_and_fingerprint_exceptions_match_and_expired_exceptions_do_not() {
    let mut raw = raw_policy("candidate", "high");
    let mut expired = exception("expired", "lint", "src/deleted.rs", None);
    expired["expires"] = json!("2026-10-31");
    raw["exceptions"] = json!([
        exception("path", "lint", "src/new.rs", None),
        exception(
            "fingerprint",
            "lint",
            "src/different.rs",
            Some(UNICODE_FINGERPRINT)
        ),
        expired
    ]);
    let policy = parsed_policy(raw);
    let assessment = evaluate(
        &policy,
        &[result(
            "lint",
            "failed",
            vec![
                finding("lint", Some("src/new.rs"), "known path", "high"),
                unicode_finding(),
                finding("lint", Some("src/deleted.rs"), "expired exemption", "high"),
            ],
            &[],
        )],
    );
    assert_eq!(assessment["findings"][0]["exception_id"], "path");
    assert_eq!(assessment["findings"][1]["exception_id"], "fingerprint");
    assert!(assessment["findings"][2]["exception_id"].is_null());
    assert_eq!(assessment["decision"], "fail");
    let mut wrong = unicode_finding();
    wrong["message"] = json!("new identity");
    let assessment = evaluate(&policy, &[result("lint", "failed", vec![wrong], &[])]);
    assert!(assessment["findings"][0]["exception_id"].is_null());
}

#[test]
fn rule_and_check_scope_are_exact_and_ambiguous_exceptions_are_refused() {
    let mut raw = raw_policy("candidate", "high");
    let mut scoped = exception("exact", "lint", "src/new.rs", None);
    scoped["rule"] = json!("only-this-rule");
    raw["exceptions"] = json!([scoped]);
    let policy = parsed_policy(raw.clone());
    let mut wrong_check = finding("other", Some("src/new.rs"), "wrong check", "high");
    wrong_check["rule_id"] = json!("only-this-rule");
    let assessment = evaluate(
        &policy,
        &[
            result(
                "lint",
                "failed",
                vec![finding("lint", Some("src/new.rs"), "wrong rule", "high")],
                &[],
            ),
            result("other", "failed", vec![wrong_check], &[]),
        ],
    );
    assert!(assessment["findings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|finding| finding["exception_id"].is_null()));
    let mut matching = finding("lint", Some("src/new.rs"), "exact scope", "high");
    matching["rule_id"] = json!("only-this-rule");
    assert_eq!(
        evaluate(
            &policy,
            &[result("lint", "failed", vec![matching.clone()], &[])]
        )["decision"],
        "pass"
    );
    raw["exceptions"].as_array_mut().unwrap().push(exception(
        "overlap",
        "lint",
        "src/new.rs",
        None,
    ));
    assert!(assess(
        &parsed_policy(raw),
        &candidate(),
        &[result("lint", "failed", vec![matching], &[])]
    )
    .is_err());
}

#[test]
fn stale_exceptions_union_shards_but_only_their_own_checks_examined_paths() {
    let mut raw = raw_policy("candidate", "high");
    raw["exceptions"] = json!([
        exception("stale-own", "lint", "src/examined.rs", None),
        exception("other-check-only", "lint", "src/other-only.rs", None),
        exception("used", "lint", "src/new.rs", None),
        exception("unexamined", "lint", "src/unseen.rs", None),
    ]);
    let assessment = evaluate(
        &parsed_policy(raw),
        &[
            result(
                "lint",
                "failed",
                vec![finding("lint", Some("src/new.rs"), "exempted", "high")],
                &["src/new.rs"],
            ),
            result("other", "passed", vec![], &["src/other-only.rs"]),
            result(
                "lint",
                "cached",
                vec![],
                &["src/examined.rs", "src/examined.rs"],
            ),
        ],
    );
    assert_eq!(assessment["decision"], "pass");
    let stale = assessment["stale_exceptions"].as_array().unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0]["id"], "stale-own");
    assert_eq!(stale[0]["check"], "lint");
    assert_eq!(stale[0]["path"], "src/examined.rs");
}

#[test]
fn supplied_order_cached_findings_and_informational_pass_results_are_preserved() {
    let assessment = evaluate(
        &policy("candidate", "high"),
        &[
            result(
                "other",
                "cached",
                vec![finding("other", Some("src/new.rs"), "cached-first", "info")],
                &[],
            ),
            result(
                "lint",
                "failed",
                vec![
                    finding("lint", Some("src/new.rs"), "second", "info"),
                    finding("lint", None, "third", "low"),
                ],
                &[],
            ),
            result(
                "lint",
                "passed",
                vec![finding(
                    "lint",
                    None,
                    "informational waiver receipt",
                    "info",
                )],
                &[],
            ),
        ],
    );
    let messages: Vec<_> = assessment["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["message"].as_str().unwrap())
        .collect();
    assert_eq!(
        messages,
        [
            "cached-first",
            "second",
            "third",
            "informational waiver receipt"
        ]
    );
    assert_eq!(assessment["decision"], "pass");
    assert_eq!(
        evaluate(&policy("candidate", "high"), &[])["review_complete"],
        false
    );
}

#[test]
fn infrastructure_and_error_results_cannot_be_inherited_or_excepted() {
    let fingerprint = "a".repeat(24);
    let mut raw = raw_policy("diff", "critical");
    raw["exceptions"] = json!([exception(
        "must-not-suppress",
        "lint",
        "src/unrelated.rs",
        Some(&fingerprint)
    )]);
    let policy = parsed_policy(raw);
    for (status, rule) in [
        ("error", "ordinary"),
        ("failed", "policy-engine-crash"),
        ("failed", "analyzer-timeout"),
        ("failed", "required-analyzer-unavailable"),
        ("failed", "analyzer-crash"),
        ("failed", "targeted-test-crash"),
        ("failed", "targeted-test-timeout"),
        ("failed", "targeted-test-killed"),
        ("failed", "slop-core-unavailable"),
    ] {
        let mut value = finding("lint", None, "infrastructure failure", "info");
        value["rule_id"] = json!(rule);
        value["fingerprint"] = json!(fingerprint);
        value["inherited"] = json!(true);
        value["exception_id"] = json!("spoofed");
        let assessment = evaluate(&policy, &[result("lint", status, vec![value], &[])]);
        assert_eq!(assessment["decision"], "fail", "{status}/{rule}");
        assert_eq!(assessment["findings"][0]["inherited"], false);
        assert!(assessment["findings"][0]["exception_id"].is_null());
    }
    let empty_error = evaluate(&policy, &[result("lint", "error", vec![], &[])]);
    assert_eq!(empty_error["decision"], "fail");
    assert_eq!(empty_error["findings"], json!([]));
}

#[test]
fn malformed_result_relationships_and_reserved_policy_checks_are_refused() {
    let policy = policy("candidate", "high");
    for results in [
        vec![result("lint", "failed", vec![], &[])],
        vec![result(
            "lint",
            "skipped",
            vec![finding("lint", None, "hidden finding", "info")],
            &[],
        )],
        vec![result(
            "lint",
            "failed",
            vec![finding("other", None, "wrong parent", "high")],
            &[],
        )],
        vec![result(
            "lint",
            "passed",
            vec![finding("lint", None, "hidden blocking finding", "high")],
            &[],
        )],
        vec![result("unknown", "passed", vec![], &[])],
    ] {
        assert!(assess(&policy, &candidate(), &results).is_err());
    }
    for reserved in ["engine-integrity", "attestation"] {
        let mut raw = raw_policy("candidate", "high");
        raw["checks"][reserved] = json!({"kind":"builtin","profiles":["fast"]});
        assert!(assess(&parsed_policy(raw), &candidate(), &[]).is_err());
    }
}

#[test]
fn reserved_engine_integrity_blocks_but_attestation_keeps_ordinary_severity_rules() {
    let policy = policy("diff", "critical");
    for status in ["passed", "failed", "error"] {
        let assessment = evaluate(
            &policy,
            &[result(
                "engine-integrity",
                status,
                vec![finding(
                    "engine-integrity",
                    None,
                    "engine evidence incomplete",
                    "info",
                )],
                &[],
            )],
        );
        assert_eq!(assessment["decision"], "fail");
        assert_eq!(assessment["findings"][0]["inherited"], false);
        assert!(assessment["findings"][0]["exception_id"].is_null());
    }
    let attestation = evaluate(
        &policy,
        &[result(
            "attestation",
            "failed",
            vec![finding(
                "attestation",
                None,
                "informational attestation evidence",
                "info",
            )],
            &[],
        )],
    );
    assert_eq!(attestation["decision"], "pass");
    assert_eq!(attestation["findings"][0]["inherited"], false);
    assert_eq!(
        evaluate(&policy, &[result("attestation", "error", vec![], &[])])["decision"],
        "fail"
    );
}

#[test]
fn individual_string_and_nested_evidence_bounds_are_refused() {
    let policy = policy("candidate", "high");
    let oversized = finding(
        "lint",
        Some("src/new.rs"),
        &"x".repeat(1024 * 1024 + 1),
        "high",
    );
    assert!(assess(
        &policy,
        &candidate(),
        &[result("lint", "failed", vec![oversized], &[])]
    )
    .is_err());
    let mut nested = json!(0);
    for _ in 0..40 {
        nested = Value::Array(vec![nested]);
    }
    let mut deep = finding("lint", Some("src/new.rs"), "deep evidence", "high");
    deep["evidence"] = json!({"nested":nested});
    assert!(assess(
        &policy,
        &candidate(),
        &[result("lint", "failed", vec![deep], &[])]
    )
    .is_err());
    assert!(assess(
        &policy,
        &candidate(),
        &[result(
            "lint",
            "failed",
            vec![finding(
                "lint",
                Some("../escape"),
                "unsafe result path",
                "high"
            )],
            &[]
        )]
    )
    .is_err());
    assert!(assess(
        &policy,
        &candidate(),
        &[result("lint", "passed", vec![], &["../escaped-file.rs"])]
    )
    .is_err());
}

#[test]
fn unrelated_check_groups_do_not_pay_a_global_exception_cross_product() {
    let mut raw = raw_policy("candidate", "high");
    let mut exceptions = Vec::new();
    for check in ["lint", "other"] {
        for index in 0..1000 {
            exceptions.push(exception(
                &format!("{check}-{index}"),
                check,
                &format!("src/{check}-{index}.rs"),
                Some("ffffffffffffffffffffffff"),
            ));
        }
    }
    raw["exceptions"] = Value::Array(exceptions);
    let policy = parsed_policy(raw);
    let results: Vec<_> = ["lint", "other"]
        .into_iter()
        .map(|check| {
            let findings = (0..1000)
                .map(|index| {
                    let mut value = finding(
                        check,
                        Some("src/new.rs"),
                        &format!("finding {index}"),
                        "info",
                    );
                    value["fingerprint"] = json!("aaaaaaaaaaaaaaaaaaaaaaaa");
                    value
                })
                .collect();
            result(check, "failed", findings, &[])
        })
        .collect();
    // Two independent 1000x1000 groups meet the two-million-pair admission cap;
    // a global 2000x2000 estimate would refuse an otherwise bounded assessment.
    let assessment = evaluate(&policy, &results);
    assert_eq!(assessment["decision"], "pass");
    assert_eq!(assessment["findings"].as_array().unwrap().len(), 2000);
    assert_eq!(assessment["stale_exceptions"], json!([]));
}

#[test]
fn empty_argv_and_file_entries_hit_count_preflight_before_content_validation() {
    let policy = policy("candidate", "high");
    let prototype = result("lint", "passed", vec![], &[]);
    for field in ["command", "files"] {
        let mut wire = serde_json::to_value(&prototype).unwrap();
        wire[field] = json!(vec![""; 100_001]);
        let oversized: CheckResult = serde_json::from_value(wire).unwrap();
        let error = assess(&policy, &candidate(), &[oversized]).unwrap_err();
        let detail = format!("{error:#}");
        assert!(detail.contains("count"), "{field}: {detail}");
        assert!(!detail.to_lowercase().contains("path"), "{field}: {detail}");
    }
    let results = vec![prototype; 20_001];
    let error = assess(&policy, &candidate(), &results).unwrap_err();
    assert!(format!("{error:#}").contains("result count"));
}

#[test]
fn wire_models_refuse_unknown_severity_status_and_fields() {
    let valid_finding = finding("lint", Some("src/new.rs"), "wire fixture", "high");
    let mut unknown_severity = valid_finding.clone();
    unknown_severity["severity"] = json!("urgent");
    assert!(serde_json::from_value::<Finding>(unknown_severity).is_err());
    let mut unknown_finding_field = valid_finding;
    unknown_finding_field["trusted"] = json!(true);
    assert!(serde_json::from_value::<Finding>(unknown_finding_field.clone()).is_err());
    let valid_result = serde_json::to_value(result("lint", "passed", vec![], &[])).unwrap();
    let mut unknown_status = valid_result.clone();
    unknown_status["status"] = json!("success");
    assert!(serde_json::from_value::<CheckResult>(unknown_status).is_err());
    let mut unknown_result_field = valid_result.clone();
    unknown_result_field["execution_verified"] = json!(true);
    assert!(serde_json::from_value::<CheckResult>(unknown_result_field).is_err());
    let mut nested_unknown = valid_result;
    nested_unknown["findings"] = json!([unknown_finding_field]);
    assert!(serde_json::from_value::<CheckResult>(nested_unknown).is_err());
}

#[test]
fn diagnostic_identifiers_cannot_inject_terminal_control_sequences() {
    let policy = policy("candidate", "high");
    for key in ["rule_id", "fingerprint", "exception_id"] {
        let mut value = finding("lint", Some("src/new.rs"), "finding", "high");
        value[key] = json!("unsafe\u{1b}[2Jidentifier");
        let error = assess(
            &policy,
            &candidate(),
            &[result("lint", "failed", vec![value], &[])],
        )
        .unwrap_err();
        let detail = format!("{error:#}");
        assert!(detail.contains("invalid assessment ID"));
        assert!(!detail.contains('\u{1b}'));
    }
}

#[test]
fn glob_work_limit_refuses_long_shared_prefixes_without_pair_or_match_shortcuts() {
    let mut raw = raw_policy("candidate", "high");
    let pattern = format!("src/module/{}b.rs", "[a-a]".repeat(800));
    let path = format!("src/module/{}c.rs", "a".repeat(800));
    raw["exceptions"] = json!([exception("costly", "lint", &pattern, None)]);
    let policy = parsed_policy(raw);
    let findings = (0..32)
        .map(|index| finding("lint", Some(&path), &format!("finding {index}"), "info"))
        .collect();
    // Every character-class prefix matches, and only the final literal differs.
    // There is one exception and no examined-file list, so the 32 pairs are far
    // below pair admission and cannot terminate through a successful match.
    let error = assess(
        &policy,
        &candidate(),
        &[result("lint", "failed", findings, &[])],
    )
    .unwrap_err();
    let detail = format!("{error:#}");
    assert!(detail.contains("glob work limit"), "{detail}");
}
