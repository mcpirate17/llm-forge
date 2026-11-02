use super::*;

pub(super) fn build(
    selection: &Selection,
    required: &[String],
    observation: collect::Observation,
    wall: f64,
    started: Instant,
) -> Result<Value> {
    let (results, incomplete) = observation.results.ordered(required)?;
    let assessment =
        crate::candidate_assessment::assess(&selection.policy, &selection.candidate, &results)?;
    let complete = incomplete.is_empty() && !observation.cancelled;
    let decision = if complete {
        assessment.decision
    } else {
        "fail"
    };
    let finished = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("review clock before epoch")?
        .as_secs_f64();
    ensure!(finished >= wall, "review wall clock moved backwards");
    let mut report = selection.output();
    report["decision"] = json!(decision);
    report["executed"] = json!(true);
    report["execution_scope"] = json!("complete_active_profile");
    report["review_complete"] = json!(complete);
    report["execution_verified"] = json!(complete);
    report["approval"] = json!(false);
    report["cancelled"] = json!(observation.cancelled);
    report["required_checks"] = json!(required);
    report["incomplete"] = json!(incomplete);
    report["checks"] = json!(results);
    report["findings"] = json!(assessment.findings);
    report["policy"]["schema_version"] = selection.policy.document["schema_version"].clone();
    report["policy"]["block_at"] = selection.policy.document["block_at"].clone();
    report["policy"]["baseline_expires"] = selection.policy.document["baseline_expires"].clone();
    report["policy"]["unmatched_exceptions"] = json!(assessment.stale_exceptions);
    report["engine"] = observation.evidence["engine"].clone();
    report["graph"] = observation.evidence["test_selection"]["graph"].clone();
    report["bypass"] = observation.evidence["bypass"].clone();
    report["evidence"] = observation.evidence;
    report["stages"] = json!(observation.stages);
    report["baselines"] = json!(observation.baselines);
    report["cache"] = json!({"mode":"disabled_fresh_execution","hits":0,"misses":0});
    report["timings"] = json!({"started_at":crate::instant::isoformat_utc(wall),
        "finished_at":crate::instant::isoformat_utc(finished),"duration_ms":started.elapsed().as_millis()});
    report["binding"] = json!({"candidate_tree_oid":selection.candidate.tree_oid,
        "base_tree_oid":selection.candidate.base_tree_oid,"profile":selection.profile(),
        "surface":selection.surface,"policy_sha256":selection.policy.digest,
        "grants_sha256":selection.grants.digest(),"required_checks":required,
        "receipt_scope":"coordinator-owned observations; checksum is not an authenticated approval"});
    // Seal only after all execution owners and collector cleanup have returned.
    crate::json_canon::encoded_size(&report, crate::candidate_receipt::MAX_BYTES)?;
    Ok(
        crate::candidate_receipt::seal(&serde_json::to_vec(&report)?)?
            .payload()
            .clone(),
    )
}
