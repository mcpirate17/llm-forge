use super::*;
use crate::confined_execution::{Outcome, Report};
use sha2::{Digest, Sha256};
fn plan() -> Plan {
    Plan {
        candidate: "1".repeat(64),
        policy: "2".repeat(64),
        grants: "3".repeat(64),
        tree: "4".repeat(40),
        id: "targeted-tests".into(),
        python: "/operator/python".into(),
        python_libdir: None,
        slop: true,
        targets: vec!["python_contracts_a".into()],
        bindings: vec![(
            "native/conductor-native/tests/python_contracts_a.rs".into(),
            "5".repeat(40),
        )],
    }
}
fn report() -> Report {
    Report {
        outcome: Outcome::Exited,
        exit_code: Some(0),
        signal: None,
        stdout: String::new(),
        stderr: String::new(),
        stdout_bytes: 0,
        stderr_bytes: 0,
        duration_ms: 1,
        errors: Vec::new(),
        cleanup_complete: true,
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
fn observation(
    p: &Plan,
    version: bool,
    report: Report,
    artifact: Option<crate::candidate_stage_artifact::Artifact>,
) -> Observation {
    Observation {
        data: json!({"candidate":p.candidate,"policy":p.policy,"grants":p.grants,"tree":p.tree,"python":p.python,"python_libdir":p.python_libdir,"slop":p.slop,"targets":p.targets,"bindings":p.bindings,"version":version}),
        report,
        artifact,
    }
}
#[test]
fn contract_versions_and_evidence_are_bound_and_complete() {
    let p = plan();
    let mut r = report();
    r.stdout = "cargo 1.98\nPython 3.12\n".into();
    r.stdout_bytes = r.stdout.len() as u64;
    assert!(version_permit(&p, &observation(&p, true, r, None)).is_ok());
    let items = ["conductor_native", "forge", "slop_core"]
        .into_iter()
        .map(|name| json!({"name":name,"sha256":"a".repeat(64),"bytes":10}))
        .collect::<Vec<_>>();
    let value = json!({"scope":"candidate_built_contract_runtime","python":p.python,"targets":p.targets,"artifacts":items,"slop_candidate_built":true,"slop_scope":"legacy_target_mapping_not_runtime_import_attestation"});
    let bytes = serde_json::to_vec(&value).unwrap();
    let captured = crate::candidate_stage_artifact::Artifact {
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        bytes,
    };
    assert!(artifact(&p, &observation(&p, false, report(), Some(captured))).is_ok());
    let value = json!({"scope":"candidate_built_contract_runtime","python":p.python,"targets":p.targets,"artifacts":[],"slop_candidate_built":true,"slop_scope":"legacy_target_mapping_not_runtime_import_attestation"});
    let bytes = serde_json::to_vec(&value).unwrap();
    let captured = crate::candidate_stage_artifact::Artifact {
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        bytes,
    };
    assert!(artifact(&p, &observation(&p, false, report(), Some(captured))).is_err());
}
#[test]
fn corrupt_evidence_and_unverified_cleanup_never_become_contract_success() {
    let p = plan();
    let mut r = report();
    r.cleanup_complete = false;
    let observed = observation(&p, true, r, None);
    assert!(version_permit(&p, &observed).is_err());
    let captured = crate::candidate_stage_artifact::Artifact {
        bytes: b"{}".to_vec(),
        sha256: "0".repeat(64),
    };
    assert!(artifact(&p, &observation(&p, false, report(), Some(captured))).is_err());
}
#[test]
fn absent_candidate_slop_is_explicitly_scoped_and_does_not_require_artifact() {
    let mut p = plan();
    p.slop = false;
    let items = ["conductor_native", "forge"]
        .into_iter()
        .map(|name| json!({"name":name,"sha256":"a".repeat(64),"bytes":10}))
        .collect::<Vec<_>>();
    let value = json!({"scope":"candidate_built_contract_runtime","python":p.python,"targets":p.targets,"artifacts":items,"slop_candidate_built":false,"slop_scope":"legacy_target_mapping_not_runtime_import_attestation"});
    let bytes = serde_json::to_vec(&value).unwrap();
    let captured = crate::candidate_stage_artifact::Artifact {
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        bytes,
    };
    let evidence = artifact(&p, &observation(&p, false, report(), Some(captured))).unwrap();
    assert_eq!(evidence["runtime"]["slop_candidate_built"], false);
    assert_eq!(
        evidence["runtime"]["slop_scope"],
        "legacy_target_mapping_not_runtime_import_attestation"
    );
}
