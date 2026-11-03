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
        record_owned: false,
        record_configs: vec![None],
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
        data: json!({"scope":"captured_pytest_shard","index":0,"version":version,"candidate_sha256":plan.candidate,"tree":plan.tree,"policy_sha256":plan.policy,"grants_sha256":plan.grants,"interpreter":plan.interpreter,"pytest":plan.pytest,"coverage_tool":plan.coverage_tool,"tests":plan.shards[0],"bindings":plan.bindings,"full":plan.full,"record_owned":plan.record_owned,"record_config":plan.record_configs[0],"snapshot_root":"/owned/one"}),
        report,
        artifact,
        record: None,
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
    let (findings, _, captured, _) = collect(
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
        let (findings, _, _, _) = collect(
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

#[test]
fn configured_record_requires_every_successful_shard_and_digest_binding() {
    let mut p = plan(true);
    p.record_owned = true;
    p.record_configs[0] = Some(RecordConfig {
        path: "pyproject.toml".into(),
        oid: "a".repeat(40),
        lines: true,
    });
    let artifact = crate::candidate_stage_artifact::Artifact {
        bytes: br#"{"files":{}}"#.to_vec(),
        sha256: format!("{:x}", Sha256::digest(br#"{"files":{}}"#)),
    };
    let mut row = obs(
        &p,
        false,
        report(Outcome::Exited, Some(0), true),
        Some(artifact),
    );
    assert!(collect(&p, "targeted-tests-full", vec![row]).is_err());
    let artifact = crate::candidate_stage_artifact::Artifact {
        bytes: br#"{"files":{}}"#.to_vec(),
        sha256: format!("{:x}", Sha256::digest(br#"{"files":{}}"#)),
    };
    row = obs(
        &p,
        false,
        report(Outcome::Exited, Some(0), true),
        Some(artifact),
    );
    row.record = Some(crate::candidate_stage_artifact::Artifact {
        bytes: b"bytes".to_vec(),
        sha256: "wrong".into(),
    });
    assert!(collect(&p, "targeted-tests-full", vec![row]).is_err());
}

#[test]
fn complete_owned_shards_retain_both_record_artifacts() {
    let mut p = plan(true);
    p.record_owned = true;
    p.record_configs[0] = Some(RecordConfig {
        path: "pyproject.toml".into(),
        oid: "a".repeat(40),
        lines: true,
    });
    p.tests.push("tests/test_b.py".into());
    p.shards.push(vec!["tests/test_b.py".into()]);
    p.record_configs.push(p.record_configs[0].clone());
    let make = |index: usize| {
        let bytes = br#"{"files":{}}"#.to_vec();
        let coverage = crate::candidate_stage_artifact::Artifact {
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            bytes,
        };
        let mut value = obs(
            &p,
            false,
            report(Outcome::Exited, Some(0), true),
            Some(coverage),
        );
        value.data["index"] = json!(index);
        value.data["tests"] = json!(p.shards[index]);
        let bytes = format!("record-{index}").into_bytes();
        value.record = Some(crate::candidate_stage_artifact::Artifact {
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            bytes,
        });
        value
    };
    let (findings, rows, coverage, records) =
        collect(&p, "targeted-tests-full", vec![make(1), make(0)]).unwrap();
    assert!(findings.is_empty());
    assert_eq!(rows.len(), 2);
    assert_eq!(coverage.len(), 2);
    assert_eq!(records.len(), 2);
    assert!(rows.iter().all(|row| row["test_record_sha256"].is_string()));
}

#[test]
fn function_is_untested_only_when_every_owned_shard_misses_it() {
    let hit_in_first = ("a.py".into(), "first".into(), 1);
    let hit_in_second = ("a.py".into(), "second".into(), 4);
    let missed = ("a.py".into(), "missed".into(), 7);
    let first = BTreeSet::from([hit_in_second.clone(), missed.clone()]);
    let second = BTreeSet::from([hit_in_first, missed.clone()]);
    assert_eq!(
        intersect_missing(Some(first), second),
        BTreeSet::from([missed])
    );
}

#[test]
fn root_recorder_setting_distinguishes_disabled_missing_and_malformed() {
    assert_eq!(record_setting(b"[tool.other]\na=1\n").unwrap(), None);
    assert_eq!(
        record_setting(b"[tool.forge.test_record]\nstore='x'\n").unwrap(),
        Some(true)
    );
    assert_eq!(
        record_setting(b"[tool.forge.test_record]\nstore='x'\nlines=false\n").unwrap(),
        Some(false)
    );
    assert!(record_setting(b"[tool.forge.test_record]\nlines=true\n").is_err());
    assert!(record_setting(b"[tool.forge.test_record]\nstore='x'\nlines='yes'\n").is_err());
}

#[test]
fn nearest_configs_partition_shards_without_dropping_disabled_or_unconfigured_tests() {
    let root = RecordConfig {
        path: "pyproject.toml".into(),
        oid: "a".repeat(40),
        lines: true,
    };
    let nested = RecordConfig {
        path: "pkg/pyproject.toml".into(),
        oid: "b".repeat(40),
        lines: false,
    };
    let settings = BTreeMap::from([
        (root.path.clone(), root.clone()),
        (nested.path.clone(), nested.clone()),
    ]);
    let tests = vec![
        "test_root.py".into(),
        "pkg/test_a.py".into(),
        "pkg/sub/test_b.py".into(),
    ];
    let (groups, configs) = group_record_shards(&tests, 1, &settings).unwrap();
    assert_eq!(groups.len(), 3);
    for (group, config) in groups.iter().zip(&configs) {
        let expected = if group[0].starts_with("pkg/") {
            &nested
        } else {
            &root
        };
        assert_eq!(config.as_ref(), Some(expected));
    }
    let (groups, configs) = group_record_shards(&tests, 10, &BTreeMap::new()).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(configs, [None]);
    let only_nested = BTreeMap::from([(nested.path.clone(), nested.clone())]);
    let (groups, configs) = group_record_shards(&tests, 10, &only_nested).unwrap();
    assert_eq!(groups.len(), 2);
    assert!(configs.contains(&None));
    assert!(configs.contains(&Some(nested)));
}
