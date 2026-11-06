use serde_json::{json, Value};

fn contract(value: &Value) -> Result<super::super::CampaignContract, String> {
    super::super::generated_campaign_contract_with(
        value.as_object().unwrap(),
        b"manifest".to_vec(),
        "campaigns/one.json",
        "fest",
        |_| None,
    )
}
fn payload() -> Value {
    json!({"campaign_id":"one","title":"fixture","language":"python","mutation_engine":"fest",
        "generator":{"source":["src/a.py"],"run_timeout_seconds":60,"jobs":1,"seed":0,
            "operators":[],"exclude":["**/test_*.py","**/conftest.py"],"options":{"execution_profile":"fest-confined-v1"}},
        "source_sha256":{"src/a.py":"a".repeat(64)},"test_sha256":{"tests/test_a.py":"b".repeat(64)},
        "test_argv":["python","-m","pytest","-q","--rootdir=.","tests/test_a.py"],
        "environment":{},"survivor_baseline":[]})
}
#[test]
fn confined_fest_contract_requires_exact_scope_and_arguments() {
    let value = payload();
    assert_eq!(
        contract(&value).unwrap().execution_profile.as_deref(),
        Some("fest-confined-v1")
    );
    for (key, changed) in [
        ("jobs", json!(2)),
        ("source", json!(["src/other.py"])),
        ("operators", json!(["something"])),
        ("exclude", json!([])),
    ] {
        let mut bad = value.clone();
        bad["generator"][key] = changed;
        assert!(contract(&bad).is_err(), "{key}");
    }
    for argv in [
        json!(["python", "-m", "pytest", "-q", "--rootdir=."]),
        json!([
            "python",
            "-m",
            "pytest",
            "-q",
            "--rootdir=.",
            "--collect-only"
        ]),
    ] {
        let mut bad = value.clone();
        bad["test_argv"] = argv;
        assert!(contract(&bad).is_err());
    }
    let mut bad = value.clone();
    bad["environment"] = json!({"PYTHONPATH":"/tmp"});
    assert!(contract(&bad).is_err());
    let mut bad = value;
    bad["generator"]["options"]["workspace_selector"] = json!({});
    assert!(contract(&bad).is_err());
}

#[test]
fn confined_profile_rejects_wrong_language_and_unknown_marker() {
    let mut bad = payload();
    bad["language"] = json!("rust");
    assert!(contract(&bad).is_err());
    let mut bad = payload();
    bad["generator"]["options"]["execution_profile"] = json!("fest-confined-v2");
    assert!(contract(&bad).is_err());
    let mut bad = payload();
    bad["generator"]["options"]["execution_profile"] = Value::Null;
    assert!(contract(&bad).is_err());
}

#[test]
fn confined_profile_refuses_nonliteral_source_and_test_paths() {
    assert!(super::validate_fest_path("src/valid_name.py").is_ok());
    for path in [
        "-option.py",
        "src/*.py",
        "src/a?.py",
        "src/{x}.py",
        "src/a:b.py",
        "src/a\\b.py",
        "src/../a.py",
        "src/a.rs",
    ] {
        assert!(super::validate_fest_path(path).is_err(), "helper {path}");
        let mut source = payload();
        source["generator"]["source"] = json!([path]);
        source["source_sha256"] = json!({(path):"a".repeat(64)});
        assert!(contract(&source).is_err(), "source {path}");
        let mut test = payload();
        test["test_argv"][5] = json!(path);
        test["test_sha256"] = json!({(path):"b".repeat(64)});
        assert!(contract(&test).is_err(), "test {path}");
    }
}
