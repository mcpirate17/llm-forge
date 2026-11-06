use super::*;

#[test]
fn probe_rejects_invalid_limits_and_guest_inside_owned_trees() {
    let mut probe = Probe {
        guest_path: Path::new("/operator/guest.so"),
        guest_sha256: "digest",
        targets: &[],
        waivers: None,
        timeout_seconds: 1.0,
        jobs: 1,
    };
    let source = Path::new("/repo");
    let scratch = Path::new("/scratch");
    assert!(validate_probe(&probe, source, scratch).is_ok());
    probe.guest_path = Path::new("/repo/guest.so");
    assert!(validate_probe(&probe, source, scratch).is_err());
    probe.guest_path = Path::new("/scratch/guest.so");
    assert!(validate_probe(&probe, source, scratch).is_err());
    probe.guest_path = Path::new("/operator/guest.so");
    probe.timeout_seconds = f64::NAN;
    assert!(validate_probe(&probe, source, scratch).is_err());
    probe.timeout_seconds = 1.0;
    probe.jobs = 0;
    assert!(validate_probe(&probe, source, scratch).is_err());
}

#[test]
fn worker_arguments_keep_pinned_paths_limits_and_optional_waivers() {
    let python = Path::new("/operator/python");
    let guest = Path::new("/operator/guest.so");
    let runtime = Path::new("/private/runtime");
    let base = worker_args(python, guest, runtime, 17.5, 2, false).unwrap();
    assert_eq!(
        base,
        vec![
            "forge",
            "equivalence-worker",
            "--python",
            "/operator/python",
            "--guest-path",
            "/operator/guest.so",
            "--targets",
            "/private/runtime/equivalence-targets.json",
            "--output",
            "/private/runtime/equivalence-summary.json",
            "--timeout-seconds",
            "17.5",
            "--jobs",
            "2",
        ]
    );
    let with_waiver = worker_args(python, guest, runtime, 17.5, 2, true).unwrap();
    assert_eq!(&with_waiver[..base.len()], base);
    assert_eq!(
        &with_waiver[base.len()..],
        ["--waivers", "/private/runtime/equivalence-waivers.json"]
    );
}
