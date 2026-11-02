use super::*;
use crate::candidate_assessment::{CheckResult, CheckStatus};
fn row(status: CheckStatus) -> CheckResult {
    CheckResult {
        check_id: "targeted-tests".into(),
        status,
        duration_ms: 0,
        findings: Vec::new(),
        files: Vec::new(),
        command: Vec::new(),
        tool_version: None,
        cache_key: None,
        cache_hit: false,
        exit_code: None,
        skipped_reason: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        metrics: serde_json::Map::new(),
    }
}
#[test]
fn matching_successful_observations_merge_without_claiming_full_review() {
    let (result, evidence) = merge(
        "targeted-tests",
        Some((row(CheckStatus::Passed), json!({"kind":"cargo"}))),
        Some((row(CheckStatus::Passed), json!({"kind":"python"}))),
        None,
        false,
    )
    .unwrap();
    assert_eq!(result.status, CheckStatus::Passed);
    assert_eq!(evidence["review_complete"], false);
    assert_eq!(evidence["python"]["kind"], "python");
}
#[test]
fn contract_and_python_failure_remain_blocking() {
    let (result, _) = merge(
        "targeted-tests",
        None,
        Some((row(CheckStatus::Passed), json!({}))),
        None,
        true,
    )
    .unwrap();
    assert_eq!(result.status, CheckStatus::Error);
    assert!(result
        .findings
        .iter()
        .any(|f| f.rule_id == "contract-runtime-unavailable"));
    let (result, _) = merge(
        "targeted-tests",
        Some((row(CheckStatus::Passed), json!({}))),
        Some((row(CheckStatus::Failed), json!({}))),
        None,
        false,
    )
    .unwrap();
    assert_eq!(result.status, CheckStatus::Failed);
}
#[test]
fn completed_contracts_remove_only_their_explicit_pending_finding() {
    let mut cargo = row(CheckStatus::Error);
    cargo
        .findings
        .push(crate::candidate_targeted_cargo::finding(
            "targeted-tests",
            "contract-runtime-unavailable",
            "not run".into(),
        ));
    let (result, evidence) = merge(
        "targeted-tests",
        Some((cargo, json!({}))),
        None,
        Some((row(CheckStatus::Passed), json!({"ran":true}))),
        true,
    )
    .unwrap();
    assert_eq!(result.status, CheckStatus::Passed);
    assert_eq!(evidence["contract"]["ran"], true);
    let (result, _) = merge(
        "targeted-tests",
        None,
        None,
        Some((row(CheckStatus::Error), json!({}))),
        true,
    )
    .unwrap();
    assert_eq!(result.status, CheckStatus::Error);
}
