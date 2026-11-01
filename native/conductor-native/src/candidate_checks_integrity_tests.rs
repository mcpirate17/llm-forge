use super::*;
use serde_json::json;
fn payload() -> Value {
    json!({"entries":[],"changes":[],"surface":"manual","protected_globs":[],"max_file_bytes":100,"max_binary_bytes":100})
}
fn change(path: &str, old: Option<&str>) -> Value {
    json!({"path":path,"old_path":old,"status":"R100","new_mode":"100644","new_oid":"abc","classes":[],"deleted":false,"exists":true})
}
#[test]
fn unicode_glob_and_three_way_order_remain_stable() {
    let mut value = payload();
    value["entries"] = json!([
        {"path":"A","mode":"100644","folded":"a","repr":"'A'"},
        {"path":"a","mode":"100600","folded":"a","repr":"'a'"},
        {"path":"elsewhere/β","mode":"100644","folded":"elsewhere/β","repr":"'elsewhere/β'"}]);
    value["changes"] = json!([change("elsewhere/β", Some("protected/α"))]);
    value["protected_globs"] = json!(["protected/[α-ω]"]);
    let result = candidate_integrity(&value).unwrap();
    let rules: Vec<_> = result["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["rule_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        rules,
        [
            "case-collision",
            "unsupported-git-mode",
            "protected-delete-or-move"
        ]
    );
    assert_eq!(
        result["findings"][2]["evidence"]["destination"],
        "elsewhere/β"
    );
    assert_eq!(result["metrics"], json!({"tree_entries":3,"changes":1}));
}
#[test]
fn admission_rejects_counts_paths_and_non_string_lists() {
    let mut value = payload();
    value["entries"] = Value::Array(vec![Value::Null; MAX_ROWS + 1]);
    assert!(candidate_integrity(&value)
        .unwrap_err()
        .contains("entry count"));
    value["entries"] = json!([]);
    value["changes"] = Value::Array(vec![Value::Null; MAX_ROWS + 1]);
    assert!(candidate_integrity(&value)
        .unwrap_err()
        .contains("change count"));
    value["changes"] = json!([change(&"x".repeat(MAX_PATH + 1), None)]);
    assert!(candidate_integrity(&value).unwrap_err().contains("path"));
    value["changes"] = json!([change("a", None)]);
    value["changes"][0]["classes"] = json!(["ok", 5]);
    assert!(candidate_integrity(&value).is_err());
    value["changes"] = json!([]);
    value["protected_globs"] = json!([1]);
    assert!(candidate_integrity(&value).is_err());
    assert!(dependency_integrity(&json!({"paths":["a",false],"snapshot":"/tmp"})).is_err());
}
#[test]
fn glob_limits_and_preflight_work_budget_fail_before_matching() {
    let mut value = payload();
    value["protected_globs"] = json!(vec!["a"; MAX_GLOBS + 1]);
    assert!(candidate_integrity(&value)
        .unwrap_err()
        .contains("glob count"));
    value["protected_globs"] = json!(["界".repeat(MAX_GLOB_CHARS + 1)]);
    assert!(candidate_integrity(&value)
        .unwrap_err()
        .contains("characters"));
    value["protected_globs"] = json!(vec!["a".repeat(4096); 16]);
    value["changes"] = json!([change(&"b".repeat(4000), None)]);
    assert!(candidate_integrity(&value)
        .unwrap_err()
        .contains("glob work"));
}
#[test]
fn aggregate_text_budget_is_checked_without_large_test_allocations() {
    let mut bytes = MAX_TEXT - 1;
    assert!(text_budget(&["ab"], &mut bytes)
        .unwrap_err()
        .contains("128 MiB"));
    let mut bytes = usize::MAX;
    assert!(text_budget(&["a"], &mut bytes)
        .unwrap_err()
        .contains("overflow"));
}
#[test]
fn matching_lock_must_exist_even_if_listed_as_changed() {
    let root = std::env::temp_dir().join(format!(
        "forge-lock-regression-{}-{}",
        std::process::id(),
        crate::native_id::new("lock").unwrap()
    ));
    std::fs::create_dir(&root).unwrap();
    let result =
        dependency_integrity(&json!({"paths":["Cargo.toml","Cargo.lock"],"snapshot":root}))
            .unwrap();
    assert_eq!(result["findings"][0]["rule_id"], "missing-lockfile");
    std::fs::write(root.join("Cargo.lock"), "owned fixture").unwrap();
    let result =
        dependency_integrity(&json!({"paths":["Cargo.toml","Cargo.lock"],"snapshot":root}))
            .unwrap();
    assert!(result["findings"].as_array().unwrap().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}
