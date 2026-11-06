use serde_json::{json, Value};

fn payload() -> Value {
    json!({"campaign_id":"one","title":"fixture","language":"cpp","mutation_engine":"mull",
        "generator":{"source":["src/a.cpp"],"run_timeout_seconds":60,"jobs":1,"seed":0,"operators":[],"exclude":[],"options":{"execution_profile":"mull-confined-v1"}},
        "source_sha256":{"src/a.cpp":"a".repeat(64)},"test_sha256":{"tests/a.cpp":"b".repeat(64)},"test_argv":["forge-mull-test"],"environment":{},"survivor_baseline":[]})
}
fn check(value: &Value) -> Result<super::super::CampaignContract, String> {
    super::super::generated_campaign_contract_with(
        value.as_object().unwrap(),
        b"manifest".to_vec(),
        "campaigns/one.json",
        "mull",
        |_| None,
    )
}
#[test]
fn confined_mull_requires_exact_generated_scope() {
    let good = payload();
    assert_eq!(
        check(&good).unwrap().execution_profile.as_deref(),
        Some("mull-confined-v1")
    );
    for (key, value) in [
        ("source", json!(["src/other.cpp"])),
        ("jobs", json!(2)),
        ("seed", json!(1)),
        ("operators", json!(["x"])),
        ("exclude", json!(["x"])),
        (
            "options",
            json!({"execution_profile":"mull-confined-v1","extra":true}),
        ),
    ] {
        let mut bad = good.clone();
        bad["generator"][key] = value;
        assert!(check(&bad).is_err(), "{key}");
    }
    for (key, value) in [
        ("test_argv", json!(["sh", "-c", "x"])),
        ("environment", json!({"PATH":"/tmp"})),
        ("language", json!("rust")),
        ("generated_value", json!({})),
    ] {
        let mut bad = good.clone();
        bad[key] = value;
        assert!(check(&bad).is_err(), "{key}");
    }
}
#[test]
fn confined_mull_rejects_bad_paths_and_mixed_language() {
    for path in [
        "src/*.cpp",
        "-test.cpp",
        "../a.cpp",
        "src/a.h",
        "src/a:b.cpp",
        "src/a\\b.cpp",
    ] {
        let mut bad = payload();
        bad["source_sha256"] = json!({(path):"a".repeat(64)});
        bad["generator"]["source"] = json!([path]);
        assert!(check(&bad).is_err(), "{path}");
    }
    let mut bad = payload();
    bad["test_sha256"] = json!({"tests/a.c":"b".repeat(64)});
    assert!(check(&bad).is_err());
}
