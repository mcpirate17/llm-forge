use super::*;
use serde_json::json;

fn receipt(survived: bool) -> Map<String, Value> {
    let outcome = if survived { "SURVIVED" } else { "KILLED" };
    let mut value = json!({
        "status": if survived {"RATCHET_HELD"} else {"PASS"},
        "source_sha256":{"src/a.cpp":"hash"},
        "mutants":[{"id":"a","path":"src/a.cpp","outcome":outcome}],
        "source_coverage":{"schema_version":1,"complete":true,"scored_by_source":{"src/a.cpp":1},"missing":[]},
        "survivors":if survived {json!(["a"])} else {json!([])},
        "outcome_counts":{"KILLED":u64::from(!survived),"SURVIVED":u64::from(survived),"NO_COVERAGE":0,"UNVIABLE":0,"ERROR":0,"TIMED_OUT":0},
        "accounting":{"planned":1,"completed":1,"failed":0,"censored":0,"unstarted":0},
        "complete_campaign":true,"mutation_score":if survived {0.0} else {1.0},
        "baseline":{"returncode":0,"timed_out":false},"engine_result":{"returncode":0,"timed_out":false}
    });
    value.as_object_mut().unwrap().clone()
}
#[test]
fn accepts_complete_pass_and_consistent_ratchet() {
    assert!(corpus_error(&receipt(false)).is_none());
    assert!(corpus_error(&receipt(true)).is_none());
}
#[test]
fn rejects_forged_status_survivor_and_score() {
    let mut bad = receipt(true);
    bad["status"] = json!("PASS");
    assert!(corpus_error(&bad).is_some());
    let mut bad = receipt(false);
    bad["status"] = json!("RATCHET_HELD");
    assert!(corpus_error(&bad).is_some());
    let mut bad = receipt(true);
    bad["survivors"] = json!([]);
    assert!(corpus_error(&bad).is_some());
    let mut bad = receipt(true);
    bad["survivors"] = json!(["a", "a"]);
    assert!(corpus_error(&bad).is_some());
    let mut bad = receipt(true);
    bad["mutation_score"] = json!(1.0);
    assert!(corpus_error(&bad).is_some());
}
#[test]
fn rejects_incomplete_outcomes_and_accounting() {
    for outcome in ["NO_COVERAGE", "TIMED_OUT", "UNVIABLE", "ERROR", "unknown"] {
        let mut bad = receipt(false);
        bad["mutants"][0]["outcome"] = json!(outcome);
        assert!(corpus_error(&bad).is_some(), "{outcome}");
    }
    for key in ["planned", "completed", "failed", "censored", "unstarted"] {
        let mut bad = receipt(false);
        bad["accounting"][key] = json!(99);
        assert!(corpus_error(&bad).is_some(), "{key}");
    }
    let mut bad = receipt(false);
    bad["outcome_counts"]["KILLED"] = json!(0);
    assert!(corpus_error(&bad).is_some());
    let mut bad = receipt(false);
    bad["complete_campaign"] = json!(false);
    assert!(corpus_error(&bad).is_some());
    let mut bad = receipt(false);
    bad["engine_result"]["returncode"] = json!(1);
    assert!(corpus_error(&bad).is_some());
    let mut bad = receipt(false);
    bad["baseline"]["timed_out"] = json!(true);
    assert!(corpus_error(&bad).is_some());
    let mut bad = receipt(false);
    bad.insert("engine_partial_progress".into(), Value::Null);
    assert!(corpus_error(&bad).is_some());
    let mut bad = receipt(false);
    bad["source_coverage"]["complete"] = json!(false);
    assert!(corpus_error(&bad).is_some());
}
