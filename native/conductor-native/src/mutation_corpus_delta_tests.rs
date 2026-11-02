use super::*;
fn empty() -> BTreeMap<String, BTreeSet<String>> {
    BASELINE_KEYS
        .iter()
        .map(|key| ((*key).to_owned(), BTreeSet::new()))
        .collect()
}
#[test]
fn all_seven_dimensions_match_and_require_complete_observations() {
    let mut found = empty();
    let mut baseline = Map::new();
    for key in BASELINE_KEYS {
        found.get_mut(key).unwrap().insert(format!("{key}-id"));
        baseline.insert(key.into(), json!([format!("{key}-id")]));
    }
    let result = compare(&found, Some(&Value::Object(baseline))).unwrap();
    assert_eq!(result["status"], "CLEAN");
    assert_eq!(result["exit_code"], 0);
    found.remove("stale_mutations");
    assert!(compare(&found, None).is_err());
}
#[test]
fn regressions_and_resolutions_are_distinct_with_patch_exit_precedence() {
    let mut found = empty();
    found
        .get_mut("uncovered_campaigns")
        .unwrap()
        .insert("new".into());
    let baseline = json!({"uncovered_campaigns":["old"],"stale_mutations":["campaign::mutant"]});
    let result = compare(&found, Some(&baseline)).unwrap();
    assert_eq!(result["status"], "REGRESSED");
    assert_eq!(result["new_uncovered_campaigns"], json!(["new"]));
    assert_eq!(result["resolved_uncovered_campaigns"], json!(["old"]));
    assert_eq!(result["exit_code"], 6);
}
#[test]
fn nonpatch_regression_or_stale_baseline_exits_seven() {
    let mut found = empty();
    found
        .get_mut("tests_that_kill_nothing")
        .unwrap()
        .insert("campaign::test".into());
    assert_eq!(compare(&found, None).unwrap()["exit_code"], 7);
    let old = json!({"uncovered_changed_files":["src/old.py"]});
    let stale = compare(&empty(), Some(&old)).unwrap();
    assert_eq!(stale["status"], "BASELINE_STALE");
    assert_eq!(stale["exit_code"], 7);
}
#[test]
fn malformed_baseline_refuses_instead_of_assuming_empty() {
    assert!(compare(&empty(), Some(&json!([]))).is_err());
    assert!(compare(&empty(), Some(&json!({"stale_mutations": [42]}))).is_err());
    assert!(compare(&empty(), Some(&json!({"stale_mutations": "bad"}))).is_err());
}
#[test]
fn observed_arrays_map_to_all_seven_deduplicated_dimensions() {
    let patches = json!({"stale":[{"campaign_id":"c","mutation_id":"m"}],"unloadable":[{"manifest":"bad.json"}]});
    let repro = json!({"interpreters":[{"campaign_id":"p"}],"evidence":[{"campaign_id":"e"}],
        "unmeasured":[{"campaign_id":"u"}],"inert_tests":[{"campaign_id":"i","nodeid":"x::y"}],
        "uncovered_files":[{"file":"src/new.rs"},{"file":"src/new.rs"}]});
    let mapped = findings(&patches, &repro).unwrap();
    assert_eq!(mapped["stale_mutations"], BTreeSet::from(["c::m".into()]));
    assert_eq!(
        mapped["tests_that_kill_nothing"],
        BTreeSet::from(["i::x::y".into()])
    );
    assert_eq!(mapped["uncovered_changed_files"].len(), 1);
    assert_eq!(mapped.len(), 7);
    assert_eq!(compare(&mapped, None).unwrap()["exit_code"], 6);
}
#[test]
fn incomplete_observation_arrays_are_rejected() {
    assert!(findings(&json!({"stale":[]}), &json!({})).is_err());
    assert!(findings(
        &json!({"stale":[{"campaign_id":"c"}],"unloadable":[]}),
        &json!({})
    )
    .is_err());
}
