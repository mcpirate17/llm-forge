use super::*;
use crate::mutation_run_test_support::{campaign, receipt as base_receipt};
use std::fs;

fn receipt(outcomes: &[(&str, &str)]) -> Receipt {
    let mut receipt = base_receipt(outcomes);
    for row in receipt
        .get_mut("mutants")
        .and_then(Value::as_array_mut)
        .expect("rows")
    {
        row["path"] = json!("src/a.py");
    }
    let pins = json!({"src/a.py": "0".repeat(64)});
    let report = conductor_native::mutation_source_coverage::report(
        pins.as_object().expect("pins"),
        receipt["mutants"].as_array().expect("rows"),
    )
    .expect("coverage report");
    receipt.insert("source_sha256".into(), pins);
    receipt.insert("source_coverage".into(), report);
    receipt.insert(
        "engine_result".into(),
        json!({"returncode": 0, "timed_out": false}),
    );
    receipt
}

fn fixture() -> (tempfile::TempDir, Campaign) {
    let dir = tempfile::tempdir().expect("tempdir");
    let campaign = campaign(dir.path(), json!({"survivor_baseline_recorded": true}));
    (dir, campaign)
}

fn scored(outcomes: &[(&str, &str)], planned: Option<u64>) -> Receipt {
    let (_dir, campaign) = fixture();
    let mut receipt = receipt(outcomes);
    score(&campaign, &mut receipt, planned).expect("scores");
    receipt
}

fn list<'a>(receipt: &'a Receipt, key: &str) -> Vec<&'a str> {
    receipt[key]
        .as_array()
        .expect("list")
        .iter()
        .map(|item| item.as_str().expect("text"))
        .collect()
}

#[test]
fn only_a_finished_all_killed_run_passes() {
    let receipt = scored(&[("a", KILLED), ("b", KILLED)], Some(2));

    assert_eq!(status(&receipt), PASS);
    assert_eq!(receipt["mutation_score"], json!(1.0));
    assert_eq!(receipt["complete_campaign"], json!(true));
}

#[test]
fn a_new_survivor_fails_and_a_known_one_only_holds() {
    let (_dir, mut campaign) = fixture();
    let mut fresh = receipt(&[("a", KILLED), ("b", SURVIVED)]);
    score(&campaign, &mut fresh, Some(2)).expect("scores");
    assert_eq!(status(&fresh), FAIL);
    assert_eq!(list(&fresh, "new_survivors"), ["b"]);
    assert_eq!(list(&fresh, "classification_required"), ["b"]);

    campaign.survivor_baseline = vec!["b".into(), "gone".into()];
    let mut known = receipt(&[("a", KILLED), ("b", SURVIVED)]);
    score(&campaign, &mut known, Some(2)).expect("scores");
    assert_eq!(status(&known), RATCHET_HELD);
    assert!(list(&known, "new_survivors").is_empty());
    assert_eq!(list(&known, "resolved_survivors"), ["gone"]);
}

#[test]
fn a_timeout_is_never_scored_as_a_kill() {
    let receipt = scored(&[("a", KILLED), ("b", TIMED_OUT)], Some(2));

    assert_eq!(status(&receipt), INCOMPLETE);
    assert_eq!(receipt["timed_out"], json!(1));
    assert_eq!(receipt["mutation_score"], json!(1.0));
    assert_eq!(receipt["accounting"]["completed"], json!(1));
    assert_eq!(receipt["accounting"]["censored"], json!(1));
}

#[test]
fn uncovered_mutants_are_counted_but_never_scored() {
    let receipt = scored(&[("a", KILLED), ("b", NO_COVERAGE)], Some(2));

    assert_eq!(status(&receipt), INCOMPLETE);
    assert_eq!(receipt["no_coverage"], json!(1));
    assert_eq!(receipt["mutation_score"], json!(1.0));
    assert_eq!(receipt["accounting"]["censored"], json!(1));
}

#[test]
fn unviable_mutants_are_counted_but_never_scored() {
    let receipt = scored(&[("a", KILLED), ("b", UNVIABLE)], Some(2));

    assert_eq!(status(&receipt), INCOMPLETE);
    assert_eq!(receipt["unviable"], json!(1));
    assert_eq!(receipt["accounting"]["failed"], json!(1));
}

#[test]
fn an_unknown_outcome_is_refused_not_averaged() {
    let (_dir, campaign) = fixture();
    let mut receipt = receipt(&[("a", KILLED), ("b", "MAYBE")]);

    let error = score(&campaign, &mut receipt, Some(2)).expect_err("unknown");

    assert_eq!(error, "unknown mutant outcome 'MAYBE'");
}

#[test]
fn an_errored_row_is_an_error_however_many_were_killed() {
    let receipt = scored(&[("a", KILLED), ("b", ROW_ERROR)], Some(2));

    assert_eq!(status(&receipt), ERROR);
    assert_eq!(receipt["complete_campaign"], json!(false));
    assert_eq!(receipt["accounting"]["failed"], json!(1));
}

#[test]
fn a_planned_mutant_with_no_row_is_incomplete() {
    let receipt = scored(&[("a", KILLED), ("b", KILLED)], Some(3));

    assert_eq!(status(&receipt), INCOMPLETE);
    assert_eq!(receipt["accounting"]["unstarted"], json!(1));
    assert_eq!(receipt["complete_campaign"], json!(false));
}

#[test]
fn a_run_with_no_plan_cannot_claim_completeness() {
    let receipt = scored(&[("a", KILLED)], None);

    assert_eq!(status(&receipt), INCOMPLETE);
    assert_eq!(receipt["accounting"]["planned"], Value::Null);
    assert_eq!(receipt["accounting"]["unstarted"], Value::Null);
}

#[test]
fn rows_beyond_the_plan_are_refused() {
    let (_dir, campaign) = fixture();
    let mut receipt = receipt(&[("a", KILLED), ("b", KILLED)]);

    let error = score(&campaign, &mut receipt, Some(1)).expect_err("over plan");

    assert_eq!(error, "the engine reported 2 mutants against a plan of 1");
}

#[test]
fn an_empty_corpus_measured_nothing() {
    let receipt = scored(&[], Some(0));

    assert_eq!(status(&receipt), INCOMPLETE);
    assert_eq!(receipt["mutation_score"], Value::Null);
}

#[test]
fn a_clean_report_from_a_failed_engine_is_not_a_pass() {
    let (_dir, campaign) = fixture();
    for engine in [
        json!({"returncode": 1, "timed_out": false}),
        json!({"returncode": null, "timed_out": false}),
        json!({"returncode": 0, "timed_out": true}),
        json!({"returncode": 0}),
    ] {
        let mut receipt = receipt(&[("a", KILLED)]);
        receipt.insert("engine_result".into(), engine.clone());
        score(&campaign, &mut receipt, Some(1)).expect("scores");
        assert_eq!(status(&receipt), INCOMPLETE, "{engine}");
    }
}

#[test]
fn the_exit_a_survivor_causes_does_not_change_the_ratchet() {
    let (_dir, mut campaign) = fixture();
    campaign.mutation_engine = "cargo-mutants".into();
    campaign.survivor_baseline = vec!["b".into()];
    let mut receipt = receipt(&[("a", KILLED), ("b", SURVIVED)]);
    receipt.insert(
        "engine_result".into(),
        json!({"returncode": 2, "timed_out": false}),
    );

    score(&campaign, &mut receipt, Some(2)).expect("scores");

    assert_eq!(status(&receipt), RATCHET_HELD);
}

#[test]
fn every_outcome_lands_in_exactly_one_account() {
    let receipt = scored(
        &[
            ("a", KILLED),
            ("b", SURVIVED),
            ("c", TIMED_OUT),
            ("d", NO_COVERAGE),
            ("e", UNVIABLE),
            ("f", ROW_ERROR),
        ],
        Some(8),
    );

    assert_eq!(
        receipt["accounting"],
        json!({"planned": 8, "completed": 2, "failed": 2, "censored": 2, "unstarted": 2})
    );
}

fn unrecorded() -> (tempfile::TempDir, Campaign) {
    let dir = tempfile::tempdir().expect("tempdir");
    let campaign = campaign(dir.path(), json!({"survivor_baseline_note": "pending"}));
    (dir, campaign)
}

#[test]
fn the_first_run_records_its_own_survivor_baseline() {
    let (_dir, mut campaign) = unrecorded();
    let before = campaign.manifest_sha256.clone();
    let mut receipt = receipt(&[("a", KILLED), ("b", SURVIVED)]);
    score(&campaign, &mut receipt, Some(2)).expect("scores");
    assert_eq!(status(&receipt), FAIL);

    let recorded = record_baseline(&mut campaign, &mut receipt, Some(2)).expect("records");

    assert!(recorded);
    assert_eq!(status(&receipt), RATCHET_HELD);
    assert_eq!(
        receipt["survivor_baseline_recorded_by_this_run"],
        json!(true)
    );
    assert_ne!(campaign.manifest_sha256, before);
    assert_eq!(receipt["manifest_sha256"], json!(campaign.manifest_sha256));
    let written: Value =
        serde_json::from_str(&fs::read_to_string(&campaign.manifest_path).expect("read"))
            .expect("json");
    assert_eq!(written["survivor_baseline"], json!(["b"]));
    assert_eq!(written["survivor_baseline_recorded"], json!(true));
    assert!(written["survivor_baseline_recorded_at"].is_string());
    assert!(written.get("survivor_baseline_note").is_none());
}

#[test]
fn a_clean_first_run_records_an_empty_baseline_so_the_ratchet_starts() {
    let (_dir, mut campaign) = unrecorded();
    let mut receipt = receipt(&[("a", KILLED)]);
    score(&campaign, &mut receipt, Some(1)).expect("scores");

    assert!(record_baseline(&mut campaign, &mut receipt, Some(1)).expect("records"));

    assert_eq!(status(&receipt), PASS);
    assert!(campaign.survivor_baseline_recorded);
}

#[test]
fn recorded_flag_and_legacy_survivors_preserve_the_ratchet() {
    for overrides in [
        json!({"survivor_baseline_recorded": true}),
        json!({"survivor_baseline": ["old"]}),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut campaign = campaign(dir.path(), overrides);
        let bytes = fs::read(&campaign.manifest_path).expect("read");
        let mut receipt = receipt(&[("new", SURVIVED)]);
        score(&campaign, &mut receipt, Some(1)).expect("scores");

        assert!(!record_baseline(&mut campaign, &mut receipt, Some(1)).expect("no record"));

        assert_eq!(status(&receipt), FAIL);
        assert_eq!(fs::read(&campaign.manifest_path).expect("read"), bytes);
    }
}

#[test]
fn an_errored_run_never_becomes_a_baseline() {
    let (_dir, mut campaign) = unrecorded();
    let bytes = fs::read(&campaign.manifest_path).expect("read");
    let mut receipt = receipt(&[("a", SURVIVED), ("b", ROW_ERROR)]);
    score(&campaign, &mut receipt, Some(2)).expect("scores");

    assert!(!record_baseline(&mut campaign, &mut receipt, Some(2)).expect("no record"));

    assert_eq!(status(&receipt), ERROR);
    assert_eq!(fs::read(&campaign.manifest_path).expect("read"), bytes);
}

#[test]
fn a_cancelled_run_never_becomes_a_baseline() {
    let (_dir, mut campaign) = unrecorded();
    let mut receipt = receipt(&[("a", SURVIVED)]);
    score(&campaign, &mut receipt, Some(1)).expect("scores");
    receipt.insert("status".into(), json!(CANCELLED));

    assert!(!record_baseline(&mut campaign, &mut receipt, Some(1)).expect("no record"));

    assert!(!campaign.survivor_baseline_recorded);
}

#[test]
fn a_run_with_unstarted_mutants_never_becomes_a_baseline() {
    for planned in [Some(3), None] {
        let (_dir, mut campaign) = unrecorded();
        let mut receipt = receipt(&[("a", SURVIVED)]);
        score(&campaign, &mut receipt, planned).expect("scores");

        assert!(!record_baseline(&mut campaign, &mut receipt, planned).expect("no record"));

        assert!(!campaign.survivor_baseline_recorded);
    }
}

#[test]
fn missing_source_coverage_prevents_a_complete_or_passing_verdict() {
    let (_dir, campaign) = fixture();
    let mut receipt = receipt(&[("a", KILLED)]);
    receipt.insert(
        "source_coverage".into(),
        json!({"complete": false, "missing": ["b.rs"]}),
    );
    score(&campaign, &mut receipt, Some(1)).unwrap();
    assert_eq!(receipt["complete_campaign"], false);
    assert_eq!(status(&receipt), INCOMPLETE);
}

#[test]
fn absent_or_forged_coverage_never_scores_as_complete() {
    let (_dir, campaign) = fixture();
    for corruption in 0..4 {
        let mut receipt = receipt(&[("a", KILLED)]);
        match corruption {
            0 => {
                receipt.remove("source_coverage");
            }
            1 => {
                receipt.insert("source_coverage".into(), json!({"complete": true}));
            }
            2 => {
                receipt.get_mut("source_coverage").unwrap()["scored_by_source"]["src/a.py"] =
                    json!(2);
            }
            _ => {
                receipt.insert("source_sha256".into(), json!({"other.py": "0".repeat(64)}));
                receipt.get_mut("mutants").unwrap()[0]["path"] = json!("other.py");
                receipt.insert("source_coverage".into(), json!({"schema_version":1,"complete":true,"scored_by_source":{"other.py":1},"missing":[]}));
            }
        }
        score(&campaign, &mut receipt, Some(1)).unwrap();
        assert_eq!(status(&receipt), INCOMPLETE, "case {corruption}");
        assert_eq!(receipt["complete_campaign"], false, "case {corruption}");
    }
}

#[test]
fn running_account_without_source_report_is_incomplete() {
    let mut receipt = base_receipt(&[("a", KILLED)]);
    account(&mut receipt, Some(1)).unwrap();
    assert_eq!(receipt["complete_campaign"], false);
}

#[test]
fn known_survivors_do_not_hide_censored_or_unviable_rows() {
    let (_dir, mut campaign) = fixture();
    campaign.survivor_baseline = vec!["known".into()];
    for outcome in [TIMED_OUT, NO_COVERAGE, UNVIABLE] {
        let mut result = receipt(&[("known", SURVIVED), ("other", outcome)]);
        score(&campaign, &mut result, Some(2)).unwrap();
        assert_eq!(status(&result), INCOMPLETE, "{outcome}");
        assert_eq!(result["complete_campaign"], false, "{outcome}");
    }
}

#[test]
fn known_survivors_do_not_hide_unfinished_or_invalid_engines() {
    let (_dir, mut campaign) = fixture();
    campaign.survivor_baseline = vec!["known".into()];
    for engine in [
        None,
        Some(json!({"returncode":0,"timed_out":true})),
        Some(json!({"returncode":null,"timed_out":false})),
        Some(json!({"returncode":0})),
        Some(json!({"returncode":1,"timed_out":false})),
    ] {
        let mut result = receipt(&[("known", SURVIVED)]);
        match &engine {
            Some(value) => {
                result.insert("engine_result".into(), value.clone());
            }
            None => {
                result.remove("engine_result");
            }
        }
        score(&campaign, &mut result, Some(1)).unwrap();
        assert_eq!(status(&result), INCOMPLETE, "{engine:?}");
        assert_eq!(result["complete_campaign"], false, "{engine:?}");
    }
}

#[test]
fn survivor_exit_two_is_only_finished_for_cargo_with_a_survivor() {
    let (_dir, mut campaign) = fixture();
    campaign.survivor_baseline = vec!["known".into()];
    for engine in ["cargo-mutants", "fest", "mull"] {
        campaign.mutation_engine = engine.into();
        let mut result = receipt(&[("known", SURVIVED)]);
        result.insert(
            "engine_result".into(),
            json!({"returncode":2,"timed_out":false}),
        );
        score(&campaign, &mut result, Some(1)).unwrap();
        assert_eq!(
            status(&result),
            if engine == "cargo-mutants" {
                RATCHET_HELD
            } else {
                INCOMPLETE
            },
            "{engine}"
        );
    }
    campaign.mutation_engine = "cargo-mutants".into();
    let mut result = receipt(&[("killed", KILLED)]);
    result.insert(
        "engine_result".into(),
        json!({"returncode":2,"timed_out":false}),
    );
    score(&campaign, &mut result, Some(1)).unwrap();
    assert_eq!(status(&result), INCOMPLETE);
}

#[test]
fn new_survivor_still_fails_despite_censored_rows() {
    let (_dir, campaign) = fixture();
    let mut result = receipt(&[("new", SURVIVED), ("other", TIMED_OUT)]);
    score(&campaign, &mut result, Some(2)).unwrap();
    assert_eq!(status(&result), FAIL);
    assert_eq!(result["complete_campaign"], false);
}
