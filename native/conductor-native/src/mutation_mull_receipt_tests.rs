use super::*;
use serde_json::json;
pub(crate) fn fixture() -> (Value, Value, Value) {
    let text = "int f(){return 1 + 1;}\n";
    let source = json!({"src/a.cpp":sha(text.as_bytes())});
    let tests = json!({"tests/a.cpp":"b".repeat(64)});
    let report = json!({"files":{"/private/worktree/src/a.cpp":{"source":text,"mutants":[{"id":"m","mutatorName":"add","replacement":"-","status":"Killed","location":{"start":{"line":1,"column":18},"end":{"line":1,"column":19}}}]}}});
    let rows = crate::mutation_mull_report::rows(
        &report,
        Path::new("/private/worktree"),
        Path::new("/private/worktree/src/a.cpp"),
        text,
    )
    .unwrap();
    let report_hash = report_sha256(&report).unwrap();
    let payload = json!({"source_sha256":source,"test_sha256":tests,"mutants":rows,"mull_definition":{"schema_version":1,"path":"/private/config","read_only":true,"removed":true},"mull_observation":{"schema_version":1,"campaign_id":"c","scope":"single-standalone-executable","source_path":"src/a.cpp","worktree":"/private/worktree","source_sha256":source,"test_sha256":tests,"report":report,"report_sha256":report_hash,"raw_report_sha256":"c".repeat(64),"executable_sha256":"d".repeat(64),"baselines":[{"returncode":0,"timed_out":false},{"returncode":0,"timed_out":false}]}});
    (payload, source, tests)
}
#[test]
fn replay_refuses_tampered_rows_pins_and_unsafe_or_incomplete_observations() {
    let (good, source, tests) = fixture();
    let check = |value: &Value| evidence_error(value.as_object().unwrap(), "c", &source, &tests);
    assert!(check(&good).is_none());
    for case in [
        "rows",
        "source",
        "driver",
        "report",
        "rawhash",
        "baseline",
        "definition",
        "path",
    ] {
        let mut bad = good.clone();
        match case {
            "rows" => bad["mutants"][0]["outcome"] = json!("SURVIVED"),
            "source" => {
                bad["mull_observation"]["source_sha256"] = json!({"src/a.cpp":"0".repeat(64)})
            }
            "driver" => {
                bad["mull_observation"]["test_sha256"] = json!({"tests/a.cpp":"0".repeat(64)})
            }
            "report" => {
                bad["mull_observation"]["report"]["files"]["/private/worktree/src/a.cpp"]
                    ["mutants"][0]["status"] = json!("Survived")
            }
            "rawhash" => bad["mull_observation"]["raw_report_sha256"] = json!("invalid"),
            "baseline" => bad["mull_observation"]["baselines"][1]["returncode"] = json!(1),
            "definition" => bad["mull_definition"]["removed"] = json!(false),
            "path" => bad["mull_observation"]["worktree"] = json!("/private/../other"),
            _ => unreachable!(),
        }
        assert!(check(&bad).is_some(), "{case}");
    }
}

#[test]
fn report_digest_survives_sorted_receipt_roundtrip_and_detects_changes() {
    let (mut payload, source, tests) = fixture();
    let report = &mut payload["mull_observation"]["report"];
    report["z_extra"] = json!({"z":1,"a":{"z":2,"a":3}});
    let hash = report_sha256(report).unwrap();
    payload["mull_observation"]["report_sha256"] = json!(hash);
    assert!(evidence_error(payload.as_object().unwrap(), "c", &source, &tests).is_none());
    payload.sort_all_objects();
    let serialized = serde_json::to_string_pretty(&payload).unwrap();
    let mut loaded: Value = serde_json::from_str(&serialized).unwrap();
    assert!(evidence_error(loaded.as_object().unwrap(), "c", &source, &tests).is_none());
    loaded["mull_observation"]["report"]["z_extra"]["a"]["z"] = json!(4);
    assert!(evidence_error(loaded.as_object().unwrap(), "c", &source, &tests).is_some());
}

#[test]
fn report_digest_refuses_oversized_input_before_canonical_copy() {
    let oversized = json!({"padding":"x".repeat(4 * 1024 * 1024)});
    assert!(report_sha256(&oversized).is_err());
}
