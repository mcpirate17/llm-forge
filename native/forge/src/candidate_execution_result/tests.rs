use super::*;
fn policy(id: &str, severity: &str) -> Value {
    conductor_native::candidate_policy::fragment(
        "check",
        &json!({"id":id,"value":{
        "kind":"command","profiles":["fast"],"severity":severity,
        "command":["operator-tool"],"version_command":["operator-tool","--version"]}}),
        "2026-11-01",
    )
    .unwrap()
}
fn report(outcome: Outcome, code: Option<i32>, stdout: &str, stderr: &str) -> Report {
    Report {
        outcome,
        exit_code: code,
        signal: None,
        stdout: stdout.into(),
        stderr: stderr.into(),
        stdout_bytes: stdout.len() as u64,
        stderr_bytes: stderr.len() as u64,
        duration_ms: 42,
        errors: Vec::new(),
        cleanup_complete: true,
        resources: crate::cgroup::Report {
            backend: "fixture",
            path: "/unused/cgroup".into(),
            cpu_quota_us: 100_000,
            cpu_period_us: 100_000,
            memory_max_bytes: 1024 * 1024,
            memory_swap_max_bytes: 0,
            processes_max: 8,
            verified: true,
        },
        cpu_seconds: Some(60),
        address_space_bytes: Some(1024 * 1024),
    }
}
fn map(value: &Value, report: &Report) -> CheckResult {
    map_result(
        value,
        &["src/b.rs".into(), "src/a.py".into(), "src/b.rs".into()],
        &["operator-tool".into(), "literal argument".into()],
        Some(&VersionEvidence::Available("tool1".into())),
        report,
    )
    .unwrap()
}
fn captured(bytes: &[u8], maximum: usize) -> Report {
    let mut tail = crate::process_capture_tail::TailBuffer::new(maximum).unwrap();
    tail.push(bytes);
    let total = tail.total_bytes();
    let mut result = report(Outcome::Exited, Some(0), &tail.finish(), "");
    result.stdout_bytes = total;
    result
}

#[test]
fn version_prefers_stdout_then_stderr_and_bounds_first_line_to_500_scalars() {
    assert_eq!(
        version(
            &report(Outcome::Exited, Some(0), "  stdout1\nsecond\n", "stderr2"),
            64
        )
        .unwrap(),
        VersionEvidence::Available("stdout1".into())
    );
    assert_eq!(
        version(
            &report(Outcome::Exited, Some(0), " \n\t", " stderr2\nnext"),
            64
        )
        .unwrap(),
        VersionEvidence::Available("stderr2".into())
    );
    assert_eq!(
        version(&report(Outcome::Exited, Some(0), "", ""), 64).unwrap(),
        VersionEvidence::Available("unknown".into())
    );
    let output = "界".repeat(501);
    assert_eq!(
        version(&report(Outcome::Exited, Some(0), &output, ""), 1024).unwrap(),
        VersionEvidence::Available("界".repeat(500))
    );
    let unicode_lines = report(Outcome::Exited, Some(0), "first\u{2028}second", "");
    assert_eq!(
        version(&unicode_lines, 64).unwrap(),
        VersionEvidence::Available("first".into())
    );
}

#[test]
fn version_completeness_rejects_discarded_prefix_and_replacement_byte_alias() {
    assert!(matches!(
        version(&captured(b"first-version\nlast", 4), 4).unwrap(),
        VersionEvidence::Unavailable(_)
    ));
    let masked = captured(b"aa\xff", 1);
    assert_eq!(masked.stdout.len() as u64, masked.stdout_bytes);
    assert!(matches!(
        version(&masked, 1).unwrap(),
        VersionEvidence::Unavailable(_)
    ));
    assert_eq!(
        version(&captured(b"tool", 4), 4).unwrap(),
        VersionEvidence::Available("tool".into())
    );
    assert_eq!(
        version(&captured(b"tool\r\nnext", 32), 32).unwrap(),
        VersionEvidence::Available("tool".into())
    );
    assert_eq!(
        version(&captured(b"ver\xff\n", 32), 32).unwrap(),
        VersionEvidence::Available("ver\u{fffd}".into())
    );
    assert!(version(&report(Outcome::Exited, Some(0), "", ""), 0).is_err());
    assert!(version(
        &report(Outcome::Exited, Some(0), "", ""),
        MAX_OUTPUT_CHARS + 1
    )
    .is_err());
}

#[test]
fn version_failures_or_unverified_cleanup_never_become_version_evidence() {
    for outcome in [Outcome::TimedOut, Outcome::Cancelled, Outcome::Failed] {
        assert!(matches!(
            version(&report(outcome, None, "tool1", ""), 32).unwrap(),
            VersionEvidence::Unavailable(_)
        ));
    }
    assert!(matches!(
        version(&report(Outcome::Exited, Some(1), "tool1", "bad"), 32).unwrap(),
        VersionEvidence::Unavailable(_)
    ));
    let mut result = report(Outcome::Exited, Some(0), "tool1", "");
    result.cleanup_complete = false;
    assert!(matches!(
        version(&result, 32).unwrap(),
        VersionEvidence::Unavailable(_)
    ));
    result.cleanup_complete = true;
    result.resources.verified = false;
    assert!(matches!(
        version(&result, 32).unwrap(),
        VersionEvidence::Unavailable(_)
    ));
    result.resources.verified = true;
    result.errors.push("cleanup warning".into());
    assert!(matches!(
        version(&result, 32).unwrap(),
        VersionEvidence::Unavailable(_)
    ));
}

#[test]
fn clean_exit_is_passed_with_sorted_files_tails_and_resource_observations() {
    let result = map(
        &policy("check", "low"),
        &report(Outcome::Exited, Some(0), "out\n", "err\n"),
    );
    assert_eq!(result.status, CheckStatus::Passed);
    assert!(result.findings.is_empty());
    assert_eq!(result.files, vec!["src/a.py", "src/b.rs"]);
    assert_eq!(result.duration_ms, 42);
    assert_eq!(result.command, vec!["operator-tool", "literal argument"]);
    assert_eq!(result.stdout_tail, "out\n");
    assert_eq!(result.stderr_tail, "err\n");
    assert_eq!(result.tool_version.as_deref(), Some("tool1"));
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.metrics["resources"]["verified"], true);
    assert_eq!(result.metrics["cleanup_complete"], true);
    assert_eq!(result.metrics["cpu_seconds"], 60);
}

#[test]
fn ordinary_exit_findings_use_configured_severity_and_legacy_message_join() {
    let result = map(
        &policy("lint", "low"),
        &report(Outcome::Exited, Some(2), " stdout\n", " stderr\n"),
    );
    assert_eq!(result.status, CheckStatus::Failed);
    let finding = &result.findings[0];
    assert_eq!(finding.rule_id, "analyzer-finding");
    assert_eq!(finding.severity, Severity::Low);
    assert_eq!(finding.message, "stdout\nstderr");
    assert_eq!(finding.evidence["exit_code"], 2);
    assert!(finding.fingerprint.is_empty());
    let blank = map(
        &policy("lint", "high"),
        &report(Outcome::Exited, Some(3), "", ""),
    );
    assert_eq!(blank.findings[0].message, "analyzer exited 3");
}

#[test]
fn missing_or_failed_version_is_critical_required_analyzer_unavailable() {
    let normalized = policy("lint", "info");
    let observation = report(Outcome::Exited, Some(0), "", "");
    for value in [
        None,
        Some(VersionEvidence::Unavailable("probe failed".into())),
    ] {
        let result = map_result(&normalized, &[], &[], value.as_ref(), &observation).unwrap();
        assert_eq!(result.status, CheckStatus::Failed);
        assert_eq!(result.findings[0].severity, Severity::Critical);
        assert_eq!(result.findings[0].rule_id, "required-analyzer-unavailable");
    }
}

#[test]
fn timeout_signal_exit126_127_and_supervision_failures_are_critical() {
    let normalized = policy("lint", "info");
    let timeout = map(&normalized, &report(Outcome::TimedOut, None, "", ""));
    assert_eq!(timeout.findings[0].rule_id, "analyzer-timeout");
    for code in [126, 127, -9] {
        let result = map(&normalized, &report(Outcome::Exited, Some(code), "", ""));
        assert_eq!(result.findings[0].rule_id, "analyzer-crash");
        assert_eq!(result.findings[0].severity, Severity::Critical);
    }
    let mut signalled = report(Outcome::Exited, None, "", "");
    signalled.signal = Some(9);
    let result = map(&normalized, &signalled);
    assert_eq!(result.exit_code, Some(-9));
    assert_eq!(result.findings[0].rule_id, "analyzer-crash");
    for outcome in [Outcome::Cancelled, Outcome::Failed] {
        assert_eq!(
            map(&normalized, &report(outcome, Some(0), "", "")).findings[0].rule_id,
            "analyzer-crash"
        );
    }
}

#[test]
fn wall_timeout_override_is_reported_separately_from_cpu_budget() {
    let mut normalized = policy("lint", "low");
    normalized["wall_timeout_override"] = json!(123);
    let timeout = map(&normalized, &report(Outcome::TimedOut, None, "", ""));
    assert_eq!(
        timeout.findings[0].message,
        "required analyzer exceeded 123s wall time"
    );
    assert_eq!(timeout.metrics["timeout_seconds"], 60);
    assert_eq!(timeout.metrics["wall_timeout_seconds"], 123);
    normalized["wall_timeout_override"] = json!(0);
    let timeout = map(&normalized, &report(Outcome::TimedOut, None, "", ""));
    assert_eq!(timeout.metrics["wall_timeout_seconds"], 60);
}

#[test]
fn cleanup_errors_and_unverified_resources_override_exit_zero() {
    let normalized = policy("lint", "info");
    for variant in 0..3 {
        let mut result = report(Outcome::Exited, Some(0), "", "");
        match variant {
            0 => result.cleanup_complete = false,
            1 => result.errors.push("cgroup cleanup failed".into()),
            _ => result.resources.verified = false,
        }
        let mapped = map(&normalized, &result);
        assert_eq!(mapped.status, CheckStatus::Failed);
        assert_eq!(mapped.findings[0].rule_id, "analyzer-crash");
        assert_eq!(mapped.findings[0].severity, Severity::Critical);
    }
    let result = map(&normalized, &report(Outcome::Exited, None, "", ""));
    assert_eq!(result.findings[0].rule_id, "analyzer-crash");
}

#[test]
fn jscpd_fingerprint_matches_independent_canonical_unicode_golden() {
    let marker =
        "ERROR: jscpd found 2 new duplicate pair(s) CAUSED by this candidate's changed files:";
    let pair_a = "  conductor/α.py  <->  conductor/β.py  (17 lines)";
    let pair_b = "  research/tools/x.py  <->  research/tools/y.py  (31 lines)";
    for (root, pairs) in [("one", [pair_a, pair_b]), ("two", [pair_b, pair_a])] {
        let output = format!(
            "audit-root: /tmp/{root}\ncommand: scratch/{root}\n{marker}\n{}\n{}\nRefactor.",
            pairs[0], pairs[1]
        );
        let result = map(
            &policy("jscpd", "high"),
            &report(Outcome::Exited, Some(1), &output, ""),
        );
        assert_eq!(result.findings[0].fingerprint, "7333b366a69b7061c8a1fdae");
        assert_eq!(result.findings[0].rule_id, "analyzer-finding");
    }
    assert_eq!(jscpd_fingerprint("unrelated audit\na <-> b (2 lines)"), "");
    assert_eq!(jscpd_fingerprint(marker), "");
}

#[test]
fn malformed_normalized_data_and_admission_overflow_fail_loudly() {
    let observation = report(Outcome::Exited, Some(0), "", "");
    let mut normalized = policy("lint", "high");
    for (field, value) in [
        ("kind", json!("builtin")),
        ("severity", json!("unknown")),
        ("timeout_seconds", json!(true)),
        ("memory_mb", json!(0)),
        ("max_output_chars", json!(MAX_OUTPUT_CHARS + 1)),
    ] {
        let mut bad = normalized.clone();
        bad[field] = value;
        assert!(
            map_result(&bad, &[], &[], None, &observation).is_err(),
            "{field}"
        );
    }
    normalized["max_output_chars"] = json!(1);
    assert!(map_result(
        &normalized,
        &[],
        &[],
        None,
        &report(Outcome::Exited, Some(0), "two", "")
    )
    .is_err());
    let argv = vec!["".to_owned(); 4097];
    assert!(map_result(&policy("lint", "high"), &[], &argv, None, &observation).is_err());
}
