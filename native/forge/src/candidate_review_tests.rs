use super::*;
use crate::candidate_assessment::CheckStatus;
use result::{result, Results};

#[test]
fn missing_obligation_becomes_error_in_requested_order() {
    let mut results = Results::default();
    results
        .insert(result("last", CheckStatus::Passed), true)
        .unwrap();
    let (rows, incomplete) = results.ordered(&["first".into(), "last".into()]).unwrap();
    assert_eq!(rows[0].check_id, "first");
    assert_eq!(rows[0].status, CheckStatus::Error);
    assert_eq!(rows[0].findings[0].rule_id, "native-review-incomplete");
    assert_eq!(rows[1].status, CheckStatus::Passed);
    assert_eq!(incomplete.len(), 1);
}
#[test]
fn duplicate_extra_or_cached_results_are_rejected() {
    let mut results = Results::default();
    results
        .insert(result("one", CheckStatus::Passed), true)
        .unwrap();
    assert!(results
        .insert(result("one", CheckStatus::Passed), true)
        .is_err());
    assert!(results.ordered(&[]).is_err());
    let mut results = Results::default();
    assert!(results
        .insert(result("one", CheckStatus::Cached), true)
        .is_err());
    let mut cached = result("one", CheckStatus::Passed);
    cached.cache_hit = true;
    assert!(results.insert(cached, true).is_err());
}
#[test]
fn executed_failure_is_complete_but_error_is_not() {
    let mut results = Results::default();
    results
        .insert(result("test", CheckStatus::Failed), true)
        .unwrap();
    assert!(results.ordered(&["test".into()]).unwrap().1.is_empty());
    let mut results = Results::default();
    results
        .insert(result("test", CheckStatus::Error), true)
        .unwrap();
    assert!(!results.ordered(&["test".into()]).unwrap().1.is_empty());
}
fn command_row() -> Value {
    json!({"check_id":"test","result":result("test", CheckStatus::Passed),
        "stages":[{"report":{"cleanup_complete":true,"outcome":"exited","exit_code":0,"resources":{"verified":true},"errors":[],"signal":null}},
                  {"report":{"cleanup_complete":true,"outcome":"exited","exit_code":0,"resources":{"verified":true},"errors":[],"signal":null}}]})
}
#[test]
fn external_result_requires_successful_version_and_both_clean_owned_stages() {
    let baseline = command_row();
    for variant in 0..11 {
        let mut row = baseline.clone();
        match variant {
            0 => (),
            1 => row["stages"][0]["report"]["exit_code"] = json!(1),
            2 => row["stages"][0]["report"]["cleanup_complete"] = json!(false),
            3 => row["stages"][1]["report"]["cleanup_complete"] = json!(false),
            4 => row["stages"][1]["report"]["outcome"] = json!("timed_out"),
            5 => row["stages"] = json!([]),
            6 => row["stages"][0]["report"]["resources"]["verified"] = json!(false),
            7 => row["stages"][1]["report"]["errors"] = json!(["resource release failure"]),
            8 => row["stages"][1]["report"]["signal"] = json!(9),
            9 => row["stages"][1]["report"]["exit_code"] = Value::Null,
            _ => {
                row["stages"].as_array_mut().unwrap().pop();
            }
        }
        let mut results = Results::default();
        results.regular("test", &row, true).unwrap();
        let (_, incomplete) = results.ordered(&["test".into()]).unwrap();
        assert_eq!(incomplete.is_empty(), variant == 0, "variant {variant}");
    }
}
#[test]
fn result_and_plan_identity_must_agree() {
    let mut row = command_row();
    row["result"]["check_id"] = json!("other");
    assert!(Results::default().regular("test", &row, true).is_err());
    assert!(Results::default().regular("other", &row, true).is_err());
}
#[test]
fn implicit_obligations_remain_blocking_without_a_public_check_row() {
    let mut results = Results::default();
    results.obligation("test-selection", "changed captured input");
    let (rows, incomplete) = results.ordered(&[]).unwrap();
    assert!(rows.is_empty());
    assert_eq!(incomplete[0]["obligation"], "test-selection");
}

#[test]
fn cleanup_and_drift_obligations_are_visible_to_existing_receipt_renderers() {
    let mut results = Results::default();
    results
        .insert(result("engine-integrity", CheckStatus::Passed), true)
        .unwrap();
    results.obligation("snapshot-cleanup", "owned directory remains");
    let (rows, incomplete) = results.ordered(&["engine-integrity".into()]).unwrap();
    assert_eq!(incomplete.len(), 1);
    assert_eq!(rows[0].status, CheckStatus::Error);
    assert!(rows[0].findings[0].message.contains("snapshot-cleanup"));
    assert_eq!(
        rows[0].findings[0].severity,
        crate::candidate_assessment::Severity::Critical
    );
}
#[test]
fn diagnostic_output_is_explicitly_bounded() {
    let row = result::error("test", "é".repeat(6000));
    assert!(row.findings[0].message.ends_with(" [truncated]"));
    assert!(row.findings[0].message.len() < 8200);
}
