use super::*;
#[test]
fn report_identifies_regressions_resolved_ids_and_prioritizes_patch_exit() {
    let mut delta = json!({"status":"REGRESSED"});
    for key in conductor_native::mutation_corpus_delta::BASELINE_KEYS {
        delta
            .as_object_mut()
            .unwrap()
            .insert(format!("new_{key}"), json!([]));
        delta
            .as_object_mut()
            .unwrap()
            .insert(format!("resolved_{key}"), json!([]));
    }
    delta["new_stale_mutations"] = json!(["a", "b", "c", "d"]);
    delta["resolved_uncovered_campaigns"] = json!(["gone"]);
    let report = json!({"status":"REGRESSED","campaigns":3,"reproducibility":{"baseline":delta},"git_build_profile":{"authority":"operator assertion"}});
    let phase = phase(report, 6).unwrap();
    assert!(!phase.ok);
    assert_eq!(phase.evidence["exit_code"], 6);
    assert!(phase.detail.contains("stale_mutations 4 (a, b, c...)"));
    assert!(phase.detail.contains("uncovered_campaigns 1 (gone)"));
}
