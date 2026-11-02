use super::*;
use crate::confined_execution::{Outcome, Report};
use sha2::{Digest, Sha256};
fn plan(full: bool) -> Plan {
    Plan {
        tests: vec!["tests/test_a.py".into()],
        shards: vec![vec!["tests/test_a.py".into()]],
        bindings: vec![("tests/test_a.py".into(), "a".repeat(40))],
        changed: vec![],
        tree: "1".repeat(40),
        policy: "2".repeat(64),
        grants: "3".repeat(64),
        candidate: "4".repeat(64),
        full,
        workers: 1,
        interpreter: "/operator/python".into(),
        pytest: "/operator/pytest".into(),
        coverage_tool: if full {
            Some("/operator/coverage".into())
        } else {
            None
        },
        threshold: 80.,
        high_threshold: 90.,
    }
}
fn report(outcome: Outcome, exit_code: Option<i32>, clean: bool) -> Report {
    Report {
        outcome,
        exit_code,
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
fn obs(
    plan: &Plan,
    version: bool,
    report: Report,
    artifact: Option<crate::candidate_stage_artifact::Artifact>,
) -> Observation {
    Observation {
        data: json!({"scope":"captured_pytest_shard","index":0,"version":version,"candidate_sha256":plan.candidate,"tree":plan.tree,"policy_sha256":plan.policy,"grants_sha256":plan.grants,"interpreter":plan.interpreter,"pytest":plan.pytest,"coverage_tool":plan.coverage_tool,"tests":plan.shards[0],"bindings":plan.bindings,"full":plan.full,"snapshot_root":"/owned/one"}),
        report,
        artifact,
    }
}
#[test]
fn policy_sharding_is_round_robin_bounded_and_complete() {
    let tests = (0..11).map(|i| format!("test_{i}.py")).collect::<Vec<_>>();
    let groups = shards(&tests, 3).unwrap();
    assert_eq!(
        groups.iter().map(Vec::len).collect::<Vec<_>>(),
        [3, 3, 3, 2]
    );
    assert_eq!(groups[0], ["test_0.py", "test_4.py", "test_8.py"]);
    assert!(shards(&tests, 0).unwrap().len() == 1);
    assert!(shards(&(0..257).map(|i| i.to_string()).collect::<Vec<_>>(), 1).is_err());
}
#[test]
fn permits_require_observed_python_pytest_and_coverage_success() {
    let p = plan(true);
    let mut r = report(Outcome::Exited, Some(0), true);
    r.stdout = "Python 3.12.3\npytest 8.3.0\nCoverage.py, version 7.6\n".into();
    r.stdout_bytes = r.stdout.len() as u64;
    assert!(version_permit(&p, &obs(&p, true, r, None)).is_ok());
    let mut r = report(Outcome::Exited, Some(0), true);
    r.stdout = "Python 3.12.3\npytest 8.3.0\n".into();
    r.stdout_bytes = r.stdout.len() as u64;
    assert!(version_permit(&p, &obs(&p, true, r, None)).is_err());
    assert!(version_permit(
        &p,
        &obs(&p, true, report(Outcome::TimedOut, None, true), None)
    )
    .is_err());
}
#[test]
fn captured_coverage_is_bound_and_failures_do_not_become_success() {
    let p = plan(true);
    let bytes = br#"{"files":{}}"#.to_vec();
    let artifact = crate::candidate_stage_artifact::Artifact {
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        bytes,
    };
    let (findings, _, captured) = collect(
        &p,
        "targeted-tests-full",
        vec![obs(
            &p,
            false,
            report(Outcome::Exited, Some(0), true),
            Some(artifact),
        )],
    )
    .unwrap();
    assert!(findings.is_empty());
    assert_eq!(captured.len(), 1);
    assert!(collect(
        &p,
        "targeted-tests-full",
        vec![obs(&p, false, report(Outcome::Exited, Some(0), true), None)]
    )
    .is_err());
    for (outcome, code, clean) in [
        (Outcome::TimedOut, None, true),
        (Outcome::Cancelled, None, true),
        (Outcome::Exited, Some(0), false),
        (Outcome::Exited, Some(1), true),
    ] {
        let (findings, _, _) = collect(
            &p,
            "targeted-tests-full",
            vec![obs(&p, false, report(outcome, code, clean), None)],
        )
        .unwrap();
        assert!(!findings.is_empty());
    }
    let bad = crate::candidate_stage_artifact::Artifact {
        bytes: b"{}".to_vec(),
        sha256: "wrong".into(),
    };
    assert!(collect(
        &p,
        "targeted-tests-full",
        vec![obs(
            &p,
            false,
            report(Outcome::Exited, Some(0), true),
            Some(bad)
        )]
    )
    .is_err());
}
