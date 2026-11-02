use super::*;
#[test]
fn observation_rejects_different_real_probe_path_or_hash() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    let snapshot = temp.path().join("snapshot");
    let operator = temp.path().join("operator");
    fs::create_dir(&snapshot).unwrap();
    fs::create_dir(&operator).unwrap();
    fs::set_permissions(&operator, fs::Permissions::from_mode(0o700)).unwrap();
    let first = operator.join("one.so");
    let second = operator.join("two.so");
    fs::write(&first, b"same").unwrap();
    fs::write(&second, b"same").unwrap();
    for path in [&first, &second] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let sha = format!("{:x}", Sha256::digest(b"same"));
    let (probe, _) =
        candidate_inputs::external_with_tmp(&first, &sha, 1024, &snapshot, None).unwrap();
    let observation = Observation {
        candidate: String::new(),
        policy: String::new(),
        requests: String::new(),
        probe,
        outcomes: vec![],
        evidence: json!({}),
    };
    observation
        .verify_probe(&Probe {
            path: first.clone(),
            sha256: sha.clone(),
        })
        .unwrap();
    assert!(observation
        .verify_probe(&Probe {
            path: second,
            sha256: sha.clone()
        })
        .is_err());
    assert!(observation
        .verify_probe(&Probe {
            path: first,
            sha256: "0".repeat(64)
        })
        .is_err());
}
#[test]
fn self_maps_requires_explicit_proc_capability() {
    use std::path::PathBuf;
    assert!(require_proc(&[]).is_err());
    assert!(require_proc(&[PathBuf::from("/proc/self")]).is_err());
    assert!(require_proc(&[PathBuf::from("/")]).is_err());
    assert!(require_proc(&[PathBuf::from("/proc")]).is_ok());
}
fn request() -> Request {
    Request {
        nodeid: "tests/test_x.py::test_one".into(),
        pinned_torch: "2.13.0+cu130".into(),
        justification: "upstream invariant".into(),
    }
}
fn measured(lines: &[(&str, u32)], native: &[&str]) -> Measured {
    Measured {
        lines: lines.iter().map(|(p, n)| (p.to_string(), *n)).collect(),
        native: native.iter().map(|s| s.to_string()).collect(),
        artifact_sha: "a".repeat(64),
    }
}
#[test]
fn exact_nodeids_pins_and_justification_are_admitted_before_execution() {
    let good = request();
    assert!(admit(std::slice::from_ref(&good)).is_ok());
    assert!(admit(&[good.clone(), good.clone()]).is_err());
    let mut invalid = good.clone();
    invalid.nodeid = "../escape.py::test_x".into();
    assert!(admit(&[invalid]).is_err());
    let mut invalid = good.clone();
    invalid.pinned_torch = String::new();
    assert!(admit(&[invalid]).is_err());
    let mut invalid = good;
    invalid.justification = " \n ".into();
    assert!(admit(&[invalid]).is_err());
}
#[test]
fn only_added_production_lines_or_sampled_native_mappings_block() {
    let r = request();
    let baseline = measured(&[("src/a.py", 1)], &[]);
    assert!(outcome(&r, &baseline, &measured(&[("src/a.py", 1)], &[])).admitted);
    assert!(
        !outcome(
            &r,
            &baseline,
            &measured(&[("src/a.py", 1), ("src/a.py", 2)], &[])
        )
        .admitted
    );
    assert!(
        !outcome(
            &r,
            &baseline,
            &measured(&[("src/a.py", 1)], &["src/ext.so"])
        )
        .admitted
    );
    assert!(
        !outcome(
            &r,
            &measured(&[], &["source-root::native.so"]),
            &measured(&[], &[])
        )
        .admitted
    );
}
