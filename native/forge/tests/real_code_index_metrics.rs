#[path = "../benches/real_code_index/metrics.rs"]
mod metrics;

#[test]
fn native_exact_reference_has_known_geometry_and_hnsw_recovers_it() -> anyhow::Result<()> {
    use conductor_native::vector_index::{Builder, Config, Query, QueryWorkspace, Space};
    let space = Space {
        dimension: 2,
        model_sha256: [7; 32],
    };
    let graph = Builder::new(
        space.clone(),
        vec![1.0, 0.0, 0.8, 0.2, 0.0, 1.0, -1.0, 0.0],
        Config::default(),
    )?;
    let query = Query::new(space, vec![1.0, 0.0])?;
    let mut scratch = QueryWorkspace::new(4, 256)?;
    let reference: Vec<_> = graph
        .exact(&query, 2, &mut scratch)?
        .hits
        .iter()
        .map(|h| h.row)
        .collect();
    assert_eq!(reference, vec![0, 1]);
    let found: Vec<_> = graph
        .search(&query, 2, 4, &mut scratch)?
        .hits
        .iter()
        .map(|h| h.row)
        .collect();
    assert_eq!(metrics::recall(&reference, &found)?, 1.0);
    Ok(())
}

mod tests {
    use super::metrics::*;
    #[test]
    fn recall_uses_exact_reference_and_unique_recovery() {
        assert_eq!(recall(&[1, 2, 3, 4], &[2, 2, 4, 9]).unwrap(), 0.5);
        assert_eq!(recall(&[1, 2], &[2, 1]).unwrap(), 1.0);
        assert_eq!(recall(&[1, 2], &[]).unwrap(), 0.0);
        assert!(recall(&[], &[1]).is_err());
        assert!(recall(&[1, 1], &[1]).is_err());
    }
    #[test]
    fn line_labels_use_bytes_and_inclusive_end() {
        assert_eq!(line_bytes("α\nb\nlast", 1, 2).unwrap(), (0, 5));
        assert_eq!(line_bytes("α\nb\nlast", 3, 3).unwrap(), (5, 9));
        assert!(line_bytes("a\n", 2, 2).is_err());
        assert!(line_bytes("a", 0, 1).is_err());
    }
    #[test]
    fn span_match_requires_path_and_positive_intersection() {
        assert!(overlaps(("a", 10, 20), ("a", 19, 30)));
        assert!(!overlaps(("a", 10, 20), ("a", 20, 30)));
        assert!(!overlaps(("a", 10, 20), ("b", 10, 20)));
    }
    #[test]
    fn frozen_workload_requires_every_query_and_label() {
        let text = include_str!("../benches/real_code_index/workload.json");
        let mut work: Workload = serde_json::from_str(text).unwrap();
        work.validate().unwrap();
        work.queries[23].relevant.clear();
        assert!(work.validate().is_err());
        let mut work: Workload = serde_json::from_str(text).unwrap();
        work.queries.pop();
        assert!(work.validate().is_err());
    }
}

mod summary_tests {
    use super::metrics::summarize;
    use serde_json::json;
    #[test]
    fn cutoff_is_unstarted_and_successful_overrun_is_censored() {
        let omitted = json!({"status":"unstarted","observation_success":null});
        let overrun = json!({"status":"censored","observation_success":true,
            "latency_us":123.0,"path_hit_at_10":true});
        let missing_latency = json!({"status":"completed","observation_success":true,
            "path_hit_at_10":true});
        let result = summarize(&[&omitted, &overrun, &missing_latency], 4);
        assert_eq!(result["completed"], 0);
        assert_eq!(result["failed"], 1);
        assert_eq!(result["censored"], 1);
        assert_eq!(result["unstarted"], 2);
        assert_eq!(result["path_hit_at_10"], 0.0);
        assert!(result["latency_us_p95_within_budget_successes_only"].is_null());
        let full = summarize(&[&omitted, &overrun, &missing_latency], 144);
        assert_eq!(full["unstarted"], 142);
        assert_eq!(full["requested"], 144);
    }
    #[test]
    fn failure_keeps_relevance_denominator_and_excludes_latency() {
        let yes = json!({"status":"completed","observation_success":true,"path_hit_at_10":true,"span_hit_at_10":true,"latency_us":10.0});
        let no = json!({"status":"completed","observation_success":true,"path_hit_at_10":false,"span_hit_at_10":false,"latency_us":30.0});
        let fail = json!({"status":"failed","observation_success":false,"error":"search failed"});
        let result = summarize(&[&yes, &no, &fail], 3);
        assert_eq!(result["completed"], 2);
        assert_eq!(result["failed"], 1);
        assert_eq!(result["path_hit_at_10"], json!(1.0 / 3.0));
        assert_eq!(result["latency_us_p95_within_budget_successes_only"], 30.0);
    }
}

mod admission_tests {
    use super::metrics::*;
    #[test]
    fn deterministic_admission_and_overrun_accounting() {
        assert_eq!(Outcome::Censored.as_str(), "censored");
        assert_eq!(
            Outcome::classify(false, None, false).unwrap(),
            Outcome::Unstarted
        );
        assert_eq!(
            Outcome::classify(true, Some(false), true).unwrap(),
            Outcome::Failed
        );
        assert_eq!(
            Outcome::classify(true, Some(true), true).unwrap(),
            Outcome::Completed
        );
        assert_eq!(
            Outcome::classify(true, Some(true), false).unwrap(),
            Outcome::Censored
        );
        assert_eq!(
            Outcome::classify(true, Some(false), false).unwrap(),
            Outcome::Censored
        );
        assert!(Outcome::classify(false, Some(true), true).is_err());
    }
    #[test]
    fn index_rejects_traversal_and_parent_symlink_into_source() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let state = temp.path().join("state");
        std::fs::create_dir(&state).unwrap();
        assert!(validated_index_path(&source, &state.join("run01")).is_ok());
        assert!(validated_index_path(&source, &state.join("../source/index")).is_err());
        let link = state.join("link");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(validated_index_path(&source, &link.join("index")).is_err());
        std::os::unix::fs::symlink(&state, state.join("final-link")).unwrap();
        assert!(validated_index_path(&source, &state.join("final-link")).is_err());
    }
}
