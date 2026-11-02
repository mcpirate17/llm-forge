use super::*;
use crate::candidate_source::{Change, Kind};
fn item(path: &str) -> TreeEntry {
    TreeEntry {
        path: path.into(),
        mode: "100644".into(),
        object_type: "blob".into(),
        oid: "1".repeat(40),
        size: Some(12),
    }
}
fn candidate(path: &str) -> Candidate {
    Candidate {
        kind: Kind::Index,
        tree_oid: "2".repeat(40),
        base_tree_oid: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
        base_commit_oid: None,
        commit_oid: None,
        target_ref: None,
        changes: vec![Change {
            status: "modified".into(),
            path: path.into(),
            old_path: None,
            old_mode: "100644".into(),
            new_mode: "100644".into(),
            old_oid: "3".repeat(40),
            new_oid: "4".repeat(40),
            classes: vec!["native".into()],
            risk: "normal".into(),
        }],
        integration_base_oid: None,
        integration_base_detail: String::new(),
    }
}
fn report(outcome: Outcome, exit: Option<i32>, clean: bool) -> Report {
    Report {
        outcome,
        exit_code: exit,
        signal: None,
        stdout: String::new(),
        stderr: String::new(),
        stdout_bytes: 0,
        stderr_bytes: 0,
        duration_ms: 1,
        errors: Vec::new(),
        cleanup_complete: clean,
        resources: crate::cgroup::Report {
            backend: "fixture",
            path: "/private".into(),
            cpu_quota_us: 100_000,
            cpu_period_us: 100_000,
            memory_max_bytes: 1024,
            memory_swap_max_bytes: 0,
            processes_max: 4,
            verified: true,
        },
        cpu_seconds: Some(1),
        address_space_bytes: Some(1024),
    }
}
fn plan(coverage: bool) -> Plan {
    Plan {
        capture_chars: 100,
        check_id: if coverage {
            "targeted-tests-full"
        } else {
            "targeted-tests"
        }
        .into(),
        tree: "1".repeat(40),
        policy_sha256: "2".repeat(64),
        grants_sha256: "4".repeat(64),
        candidate_sha256: "3".repeat(64),
        jobs: vec![Job {
            manifest: "crate/Cargo.toml".into(),
            manifest_oid: "1".repeat(40),
            files: vec![],
        }],
        python_pending: false,
        contracts_pending: false,
        coverage_required: coverage,
        coverage: None,
    }
}
#[test]
fn exact_source_and_test_blobs_are_grouped_by_captured_manifest() {
    let entries = [
        "crate/Cargo.toml",
        "crate/src/lib.rs",
        "crate/tests/check.rs",
    ]
    .into_iter()
    .map(|path| (path.to_owned(), item(path)))
    .collect();
    let native = Map::from_iter([("crate/src/lib.rs".into(), json!(["crate/tests/check.rs"]))]);
    let jobs = prepare_jobs(&entries, &candidate("crate/src/lib.rs"), &native).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].manifest, "crate/Cargo.toml");
    assert_eq!(
        jobs[0]
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["crate/src/lib.rs", "crate/tests/check.rs"]
    );
    assert!(jobs[0].files.iter().all(|file| file.oid == "1".repeat(40)));
    assert!(prepare_jobs(&entries, &candidate("other.rs"), &native).is_err());
}
#[test]
fn missing_grant_and_cross_crate_or_missing_test_are_refused() {
    assert!(granted_program(&BTreeMap::new()).is_err());
    let entries = [
        "crate/Cargo.toml",
        "crate/src/lib.rs",
        "other/Cargo.toml",
        "other/tests/check.rs",
    ]
    .into_iter()
    .map(|path| (path.to_owned(), item(path)))
    .collect();
    let native = Map::from_iter([("crate/src/lib.rs".into(), json!(["other/tests/check.rs"]))]);
    assert!(prepare_jobs(&entries, &candidate("crate/src/lib.rs"), &native).is_err());
    let missing = Map::from_iter([("crate/src/lib.rs".into(), json!(["crate/tests/missing.rs"]))]);
    assert!(prepare_jobs(&entries, &candidate("crate/src/lib.rs"), &missing).is_err());
}
#[test]
fn zero_exit_can_pass_fast_scope_but_not_full_coverage() {
    assert!(
        observation_problem("targeted-tests", &report(Outcome::Exited, Some(0), true)).is_none()
    );
    let regular = scope_findings("targeted-tests", &plan(false));
    assert!(regular.is_empty());
    let full = scope_findings("targeted-tests-full", &plan(true));
    assert!(full
        .iter()
        .any(|finding| finding.rule_id == "coverage-incomplete"));
}
#[test]
fn selected_python_or_contract_obligations_still_block_fast() {
    let mut plan = plan(false);
    plan.python_pending = true;
    plan.contracts_pending = true;
    let findings = scope_findings("targeted-tests", &plan);
    assert!(findings
        .iter()
        .any(|finding| finding.rule_id == "pytest-adapter-unavailable"));
    assert!(findings
        .iter()
        .any(|finding| finding.rule_id == "contract-runtime-unavailable"));
}

fn observation(version: bool, outcome: Outcome, code: Option<i32>) -> Observation {
    let mut report = report(outcome, code, true);
    if version {
        report.stdout = "cargo 1.98.0 (fixture)\n".into();
        report.stdout_bytes = report.stdout.len() as u64;
    }
    Observation {
        index: 0,
        version,
        report,
        argv: vec!["forge".into(), "candidate".into(), "cargo-test".into()],
        data: json!({"tree":"1".repeat(40),"candidate_sha256":"3".repeat(64),"policy_sha256":"2".repeat(64),"grants_sha256":"4".repeat(64),"bindings":plan(false).jobs[0].files,"manifest":"crate/Cargo.toml","manifest_oid":"1".repeat(40),"version":version,"coverage":false,"snapshot_root":"/private/snapshot","cargo_version":if version {None} else {Some("cargo 1.98.0 (fixture)")}}),
        artifact: None,
    }
}
#[test]
fn successful_version_then_zero_test_exit_has_no_fast_findings() {
    let plan = plan(false);
    let version = observation(true, Outcome::Exited, Some(0));
    assert_eq!(
        accept_version(&plan, &version).unwrap().version,
        "cargo 1.98.0 (fixture)"
    );
    let (rows, findings) = collect_observations(
        "targeted-tests",
        &plan,
        vec![version, observation(false, Outcome::Exited, Some(0))],
    )
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(findings.is_empty());
}
#[test]
fn failed_version_never_authorizes_a_test_stage() {
    let plan = plan(false);
    let version = observation(true, Outcome::TimedOut, None);
    assert!(accept_version(&plan, &version).is_err());
    let (rows, findings) = collect_observations("targeted-tests", &plan, vec![version]).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(findings[0].rule_id, "required-analyzer-unavailable");
    assert!(collect_observations(
        "targeted-tests",
        &plan,
        vec![
            observation(true, Outcome::TimedOut, None),
            observation(false, Outcome::Exited, Some(0))
        ]
    )
    .is_err());
}
#[test]
fn nonzero_timeout_cancellation_and_cleanup_refuse_success() {
    for (outcome, code, clean, rule) in [
        (Outcome::Exited, Some(3), true, "targeted-test-failed"),
        (Outcome::TimedOut, None, true, "targeted-test-timeout"),
        (Outcome::Cancelled, None, true, "targeted-test-killed"),
        (Outcome::Exited, Some(0), false, "targeted-test-crash"),
    ] {
        assert_eq!(
            observation_problem("targeted-tests", &report(outcome, code, clean))
                .unwrap()
                .rule_id,
            rule
        );
    }
}
#[test]
fn full_rust_coverage_requires_matching_tools_and_bound_lcov() {
    use crate::candidate_python_coverage::ChangedFile;
    let mut p = plan(true);
    p.coverage = Some(crate::candidate_targeted_rust_coverage::Plan {
        cov: "/tool/llvm-cov".into(),
        profdata: "/tool/llvm-profdata".into(),
        files: std::collections::BTreeMap::from([(
            "crate/Cargo.toml".into(),
            vec![ChangedFile {
                path: "crate/src/lib.rs".into(),
                absolute_path: String::new(),
                lines: [3, 4].into_iter().collect(),
                high_risk: false,
            }],
        )]),
        unsupported: vec![],
        high: 90.,
        other: 50.,
    });
    let mut version = observation(true, Outcome::Exited, Some(0));
    version.data["coverage"] = json!(true);
    assert!(accept_version(&p, &version).is_err());
    version.report.stdout =
        "cargo 1.98.0\nrustc LLVM 22.1; llvm-profdata and llvm-cov matched\n".into();
    version.report.stdout_bytes = version.report.stdout.len() as u64;
    assert!(accept_version(&p, &version).is_ok());
    let mut test = observation(false, Outcome::Exited, Some(0));
    test.data["coverage"] = json!(true);
    test.data["cargo_version"] = json!("cargo 1.98.0");
    let bytes =
        b"TN:\nSF:/private/snapshot/crate/src/lib.rs\nDA:3,1\nDA:4,0\nend_of_record\n".to_vec();
    test.artifact = Some(crate::candidate_stage_artifact::Artifact {
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        bytes,
    });
    let (rows, findings) =
        collect_observations("targeted-tests-full", &p, vec![version, test]).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["coverage"]["other"]["covered"], 1);
    assert!(findings.is_empty());
}
