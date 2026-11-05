use super::*;
use serde_json::{json, Value};

fn declaration() -> Value {
    json!({"schema_version":1,"target":{"manifest_path":"native/Cargo.toml","package":"forge","test":"integration"},
        "baseline_repetitions":2,"required_contracts":[{"id":"safety","criticality":"high","active_paths":["src/a.rs","src/b.rs"]}],
        "ranked_tests":[{"rank":1,"nodeid":"tests/value.rs::checks::first","contract":"safety","rationale":"covers safety"}]})
}
fn selected() -> Vec<String> {
    vec!["src/b.rs".into(), "src/a.rs".into()]
}
fn check(value: &Value) -> Result<Declaration, String> {
    parse(&serde_json::to_vec(value).unwrap(), &selected())
}
#[test]
fn accepts_explicit_contract_and_defaults_optional_redundancy() {
    let value = check(&declaration()).unwrap();
    assert_eq!(value.target().test(), "integration");
    assert_eq!(value.baseline_repetitions(), 2);
    assert_eq!(value.required_contracts()[0].criticality(), "high");
    assert!(!value.ranked_tests()[0].intentional_redundancy());
}
#[test]
fn rejects_wrong_scope_duplicates_and_ambiguity() {
    let v = declaration();
    assert!(parse(&serde_json::to_vec(&v).unwrap(), &["src/a.rs".into()]).is_err());
    assert!(parse(
        &serde_json::to_vec(&v).unwrap(),
        &["src/a.rs".into(), "src/a.rs".into()]
    )
    .is_err());
    let mut v = declaration();
    v["required_contracts"][0]["active_paths"] = json!(["src/a.rs", "src/a.rs"]);
    assert!(check(&v).is_err());
    let mut v = declaration();
    let duplicate = v["required_contracts"][0].clone();
    v["required_contracts"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(check(&v).is_err());
    let mut v = declaration();
    let mut other = v["ranked_tests"][0].clone();
    other["rank"] = json!(2);
    other["nodeid"] = json!("tests/other.rs::checks::first");
    v["ranked_tests"].as_array_mut().unwrap().push(other);
    assert!(check(&v).is_err());
}
#[test]
fn rejects_malformed_targets_paths_rank_and_contract() {
    for (field, value) in [
        ("manifest_path", "../Cargo.toml"),
        ("manifest_path", "native/other.toml"),
        ("package", "--bad"),
        ("test", ""),
    ] {
        let mut v = declaration();
        v["target"][field] = json!(value);
        assert!(check(&v).is_err(), "{field}");
    }
    for node in [
        "../tests/t.rs::valid",
        "tests/t.rs::",
        "tests/t.rs::bad-name",
        "tests/t.py::valid",
    ] {
        let mut v = declaration();
        v["ranked_tests"][0]["nodeid"] = json!(node);
        assert!(check(&v).is_err(), "{node}");
    }
    for (field, value) in [
        ("rank", json!(2)),
        ("contract", json!("other")),
        ("rationale", json!("")),
    ] {
        let mut v = declaration();
        v["ranked_tests"][0][field] = value;
        assert!(check(&v).is_err(), "{field}");
    }
    for criticality in ["medium", "", "urgent"] {
        let mut v = declaration();
        v["required_contracts"][0]["criticality"] = json!(criticality);
        assert!(check(&v).is_err());
    }
}
#[test]
fn rejects_unknown_duplicate_fields_schema_and_limits() {
    let mut v = declaration();
    v["target"]["unknown"] = json!(true);
    assert!(check(&v).is_err());
    let bytes = serde_json::to_string(&declaration()).unwrap().replacen(
        "\"schema_version\":1",
        "\"schema_version\":1,\"schema_version\":1",
        1,
    );
    assert!(parse(bytes.as_bytes(), &selected()).is_err());
    for repetitions in [1, 6] {
        let mut v = declaration();
        v["baseline_repetitions"] = json!(repetitions);
        assert!(check(&v).is_err());
    }
    let mut v = declaration();
    v["schema_version"] = json!(2);
    assert!(check(&v).is_err());
    assert!(parse(&vec![b' '; MAX_DOCUMENT + 1], &selected()).is_err());
}

#[test]
fn intentional_redundancy_cannot_be_self_declared_for_admission() {
    let mut value = declaration();
    value["ranked_tests"][0]["intentional_redundancy"] = json!(false);
    assert!(check(&value).is_ok());
    value["ranked_tests"][0]["intentional_redundancy"] = json!(true);
    let error = check(&value).unwrap_err();
    assert!(error.contains("intentional redundancy"));
}
