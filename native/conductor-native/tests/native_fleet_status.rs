//! Data-only fleet aggregation and rendering, without Python or live processes.
use conductor_native::fleet_status::{build_report, render};
use serde_json::{json, Map, Value};

fn object(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[test]
fn report_unions_all_seat_sources_and_preserves_observations() {
    let heard = object(json!({"heard-seat":{"at":"t","said":"hello"}}));
    let processes = object(json!({"/tmp/tree":["worker"]}));
    let report = build_report(
        &object(json!({"peer-seat":{"status":"up","port":7001}})),
        &heard,
        &object(
            json!({"active_claims":[{"owner":"claim-seat","paths":["a.rs"]}],
            "active_headings":["ready — timestamp, heading-seat","no seat marker",
                "invalid, ../escape","invalid, two words","invalid, ",42]}),
        ),
        &processes,
        "generated-time",
        "/host",
    );
    assert_eq!(
        report["seats"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["claim-seat", "heading-seat", "heard-seat", "peer-seat"]
    );
    assert_eq!(report["seats"]["peer-seat"]["a2a"], "up:7001");
    assert_eq!(report["seats"]["heard-seat"]["a2a"], "NO IDENTITY");
    assert_eq!(
        report["seats"]["heard-seat"]["last_heard"],
        heard["heard-seat"]
    );
    assert_eq!(
        report["seats"]["heading-seat"]["headings"],
        json!(["ready — timestamp, heading-seat"])
    );
    assert_eq!(report["worktree_processes"], json!(processes));
    assert_eq!(report["generated_at"], "generated-time");
    assert_eq!(report["root"], "/host");
}

#[test]
fn claims_deduplicate_sorted_paths_and_choose_earliest_nonempty_expiry() {
    let report = build_report(
        &object(json!({"seat":{"status":"down"},"unregistered":null})),
        &Map::new(),
        &object(json!({"active_claims":[
            {"owner":"seat","paths":["z.rs","a.rs","z.rs",42],"expires_at":"2026-09-03T00:00:00+00:00"},
            {"owner":"seat","paths":["b.rs","a.rs"],"expires_at":"2026-09-02T00:00:00+00:00"},
            {"owner":"seat","paths":[],"expires_at":""},
            {"owner":"other","paths":["foreign.rs"],"expires_at":"2026-01-01"},
            {"owner":"","paths":["ignored.rs"]},null]})),
        &Map::new(),
        "t",
        "/host",
    );
    let seat = &report["seats"]["seat"];
    assert_eq!(seat["claims"], 3);
    assert_eq!(seat["claim_paths"], json!(["a.rs", "b.rs", "z.rs"]));
    assert_eq!(seat["soonest_expiry"], "2026-09-02T00:00:00+00:00");
    assert_eq!(seat["a2a"], "down");
    assert_eq!(report["seats"]["unregistered"]["a2a"], "NO IDENTITY");
    assert_eq!(report["seats"]["unregistered"]["claims"], 0);
    assert!(report["seats"]["unregistered"]["soonest_expiry"].is_null());
    assert!(report["seats"].get("").is_none());
}

#[test]
fn earliest_expiry_compares_utc_instants_and_ignores_invalid_timestamps() {
    let report = build_report(
        &Map::new(),
        &Map::new(),
        &object(json!({"active_claims":[
            {"owner":"seat","expires_at":"2026-09-02T09:00:00Z"},
            {"owner":"seat","expires_at":"2026-09-02T10:00:00+02:00"},
            {"owner":"seat","expires_at":"2026-09-02T02:00:00-07:00"},
            {"owner":"seat","expires_at":"0000-invalid"},
            {"owner":"seat","expires_at":""},
            {"owner":"invalid-only","expires_at":"2026-09-02"},
            {"owner":"invalid-only","expires_at":"2026-09-02T01:00:00+99:00"}]})),
        &Map::new(),
        "t",
        "/host",
    );
    assert_eq!(
        report["seats"]["seat"]["soonest_expiry"],
        "2026-09-02T10:00:00+02:00"
    );
    assert_eq!(report["seats"]["seat"]["claims"], 5);
    assert!(report["seats"]["invalid-only"]["soonest_expiry"].is_null());
    assert_eq!(report["seats"]["invalid-only"]["claims"], 2);
}

#[test]
fn rendering_caps_paths_and_only_details_temporary_worktree_processes() {
    let report = object(json!({"generated_at":"t","root":"/host",
        "seats":{"seat":{"a2a":"down","claims":1,"claim_paths":["p1","p2","p3","p4","p5","p6"]}},
        "worktree_processes":{"/tmp/scratch":["proc1","proc2","proc3","proc4","proc5"],
            "/host":["daemon1","daemon2"]}}));
    let output = render(&report).unwrap();
    assert!(output.contains("p1, p2, p3, p4 (+2 more)"));
    assert!(!output.contains("p5"));
    assert!(output.contains("/tmp/scratch: 5"));
    assert!(output.contains("proc1") && output.contains("proc3"));
    assert!(!output.contains("proc4"));
    assert!(output.contains("… +2 more"));
    assert!(output.contains("/host: 2"));
    assert!(!output.contains("daemon1"));
}

#[test]
fn rendering_preserves_expiry_offset_and_truncates_heard_time_by_character() {
    let report = object(json!({"seats":{"seat":{"a2a":"NO IDENTITY","claims":0,
        "soonest_expiry":"2026-09-02T10:00:00+02:00","last_heard":{"at":"é".repeat(20),"said":"Unicode 🦀 text"}}}}));
    let output = render(&report).unwrap();
    assert!(output.contains("soonest-expiry=2026-09-02T10:00:00+02:00"));
    assert!(!output.contains("soonest-expiry=2026-09-02T10:00Z"));
    assert!(!output.contains("+02:00Z"));
    assert!(output.contains(&format!("last heard {}: Unicode 🦀 text", "é".repeat(19))));
    assert!(!output.contains(&"é".repeat(20)));
    assert!(output.contains("[NO IDENTITY]  claims=0"));
}

#[test]
fn malformed_seat_is_an_explicit_error_and_empty_report_is_renderable() {
    for seat in [Value::Null, json!([]), json!(1), json!("invalid")] {
        let error = render(&object(json!({"seats":{"bad-seat":seat}}))).unwrap_err();
        assert_eq!(error, "fleet status seat \"bad-seat\" must be an object");
    }
    let report = build_report(
        &Map::new(),
        &Map::new(),
        &Map::new(),
        &Map::new(),
        "t",
        "/host",
    );
    assert_eq!(report["seats"], json!({}));
    assert_eq!(
        render(report.as_object().unwrap()).unwrap(),
        "FLEET STATUS  t  root=/host"
    );
}

#[test]
fn probe_budget_exhaustion_stays_distinct_from_observed_failure() {
    let report = build_report(
        &object(json!({"offline":{"status":"down","port":7001},
            "budget-exhausted":{"status":"unprobed","port":7002}})),
        &Map::new(),
        &Map::new(),
        &Map::new(),
        "t",
        "/host",
    );
    assert_eq!(report["seats"]["offline"]["a2a"], "down");
    assert_eq!(report["seats"]["budget-exhausted"]["a2a"], "unprobed");
    let output = render(report.as_object().unwrap()).unwrap();
    assert!(output.contains("● offline  [down]"));
    assert!(output.contains("● budget-exhausted  [unprobed]"));
}
