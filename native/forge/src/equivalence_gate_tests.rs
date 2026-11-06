use super::*;
use clap::Parser;
use git2::{Repository, Signature};
use std::{fs, os::unix::fs::PermissionsExt};

#[path = "equivalence_gate_bench.rs"]
mod bench;

#[derive(Parser)]
struct Command {
    #[command(flatten)]
    gate: GateArgs,
}

fn args() -> GateArgs {
    Command::parse_from([
        "gate",
        "--commit",
        "0123456789012345678901234567890123456789",
        "--module",
        "src/probe.py",
        "--grants",
        "/operator/grants.json",
        "--guest-path",
        "/operator/guest.so",
        "--guest-sha256",
        &"a".repeat(64),
    ])
    .gate
}

struct Fixture {
    _root: tempfile::TempDir,
    source: Source,
    grants: Grants,
    commit: String,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let repository = Repository::init(root.path().join("repo")).unwrap();
        let mut index = repository.index().unwrap();
        index.write().unwrap();
        let tree = index.write_tree().unwrap();
        let signature = Signature::now("fixture", "fixture@example.invalid").unwrap();
        let commit = repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "empty committed fixture",
                &repository.find_tree(tree).unwrap(),
                &[],
            )
            .unwrap()
            .to_string();
        for name in ["scratch", "cgroup"] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        let source = Source::open(repository.workdir().unwrap()).unwrap();
        let path = root.path().join("grants.json");
        fs::write(&path, serde_json::to_vec(&json!({
            "version":1, "source_root":source.root(),
            "read_only":[source.root()], "programs":{}, "environment":{},
            "scratch_parent":root.path().join("scratch"), "cgroup_parent":root.path().join("cgroup"),
            "resources":{"cpu_millis":1000,"memory_mib":512,"processes":16},
            "max_wall_seconds":60,"max_cpu_seconds":30,"max_address_space_mib":1024,
            "batch":{"max_workers":1,"cpu_millis":1000,"memory_mib":512,"processes":16}
        })).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let grants = Grants::load(&path, &source).unwrap();
        Self {
            _root: root,
            source,
            grants,
            commit,
        }
    }
}

#[test]
fn literal_selection_refuses_refs_abbreviations_traversal_and_invalid_jobs() {
    let mut selected = args();
    validate_selection(&selected).unwrap();
    for commit in [
        "HEAD",
        "main",
        "0123456",
        "012345678901234567890123456789012345678G",
    ] {
        selected.commit = Some(commit.into());
        assert!(validate_selection(&selected).is_err(), "{commit}");
    }
    selected = args();
    for module in [
        "/probe.py",
        "../probe.py",
        "src/./probe.py",
        "src//probe.py",
        "probe.rs",
        "probe\n.py",
    ] {
        selected.module = module.into();
        assert!(validate_selection(&selected).is_err(), "{module}");
    }
    selected = args();
    for jobs in [0, 33, usize::MAX] {
        selected.jobs = jobs;
        assert!(validate_selection(&selected).is_err());
    }
}

#[test]
fn committed_object_resolution_refuses_tree_ids_and_missing_objects() {
    let f = Fixture::new();
    let tree = f.source.commit_tree(&f.commit).unwrap();
    assert!(f.source.commit_tree(&tree).is_err());
    assert!(f.source.commit_tree(&"f".repeat(40)).is_err());
}

#[test]
fn execution_limits_respect_real_operator_grants_and_batch_admission() {
    let mut f = Fixture::new();
    let mut selected = args();
    let limits = execution_limits(&selected, &f.grants).unwrap();
    assert_eq!(limits.timeout, Duration::from_secs(60));
    assert_eq!(limits.cpu_seconds, Some(30));
    assert_eq!(limits.address_space_bytes, Some(1024 * 1024 * 1024));
    for invalid in [0, 61] {
        selected.wall_seconds = Some(invalid);
        assert!(execution_limits(&selected, &f.grants).is_err());
    }
    selected.wall_seconds = None;
    for invalid in [0, 31] {
        selected.cpu_seconds = Some(invalid);
        assert!(execution_limits(&selected, &f.grants).is_err());
    }
    selected.cpu_seconds = None;
    for invalid in [0, 1025] {
        selected.address_space_mib = Some(invalid);
        assert!(execution_limits(&selected, &f.grants).is_err());
    }
    selected.address_space_mib = None;
    f.grants.batch.as_mut().unwrap().memory_mib = 256;
    assert!(execution_limits(&selected, &f.grants).is_err());
}

#[test]
fn capture_cleanup_failure_cannot_become_a_success_or_hide_the_primary_error() {
    let completed = Completed {
        summary: Summary::default(),
        bytes: b"{}".to_vec(),
        evidence: json!({}),
        artifacts: None,
    };
    let error = finish_capture(
        Ok(completed),
        Err(anyhow::anyhow!("owned snapshot remains")),
    )
    .err()
    .unwrap();
    assert!(format!("{error:#}").contains("owned snapshot remains"));
    let error = finish_capture::<Completed>(
        Err(anyhow::anyhow!("guest failed")),
        Err(anyhow::anyhow!("cleanup failed")),
    )
    .err()
    .unwrap();
    let text = format!("{error:#}");
    assert!(text.contains("guest failed") && text.contains("cleanup failed"));
}

#[test]
fn source_modes_require_explicit_live_inputs_and_refuse_mixed_authority() {
    let common = [
        "gate",
        "--module",
        "probe.py",
        "--grants",
        "/operator/grants.json",
        "--guest-path",
        "/operator/guest.so",
        "--guest-sha256",
        "invalid-not-loaded",
    ];
    for extra in [
        Vec::<&str>::new(),
        vec!["--live"],
        vec![
            "--commit",
            "0123456789012345678901234567890123456789",
            "--input",
            "src",
        ],
        vec![
            "--commit",
            "0123456789012345678901234567890123456789",
            "--live",
            "--input",
            "src",
        ],
    ] {
        assert!(
            Command::try_parse_from(common.iter().copied().chain(extra.iter().copied())).is_err(),
            "unexpectedly accepted source flags: {extra:?}"
        );
    }
    let selected = Command::try_parse_from(
        common
            .iter()
            .copied()
            .chain(["--live", "--input", "src", "--input", "tests"]),
    )
    .unwrap()
    .gate;
    assert!(selected.live && selected.commit.is_none());
    assert_eq!(selected.inputs, ["src", "tests"]);
    validate_selection(&selected).unwrap();
}

#[test]
fn live_source_adapter_keeps_origin_and_binds_selected_disk_inputs() {
    let f = Fixture::new();
    let origin = f.source.root().to_owned();
    fs::create_dir(origin.join("tests")).unwrap();
    fs::write(origin.join("probe.py"), "def value():\n    return 17\n").unwrap();
    fs::write(
        origin.join("tests/test_probe.py"),
        "from probe import value\ndef test_value():\n    assert value() == 17\n",
    )
    .unwrap();
    fs::write(
        origin.join("pyproject.toml"),
        "[tool.forge.equivalence_probe]\njobs=1\n",
    )
    .unwrap();
    let mut requested = args();
    requested.commit = None;
    requested.live = true;
    requested.inputs = vec!["probe.py".into(), "tests".into()];
    let selected = SelectedSource::capture(&requested, f.source, &f.grants).unwrap();
    assert_eq!(selected.origin().root(), origin);
    assert_ne!(selected.tree_source().root(), origin);
    assert_eq!(selected.scope(), "live-whole-module-declared-inputs");
    assert!(selected.live_evidence().is_some());
    let entries = selected.tree_source().list_tree(selected.tree()).unwrap();
    assert!(entries.iter().any(|entry| entry.path == "pyproject.toml"));
    selected.verify().unwrap();
    fs::write(origin.join("probe.py"), "def value():\n    return 99\n").unwrap();
    assert!(selected.verify().is_err());
    selected.discard().unwrap();
    assert_eq!(fs::read_dir(&f.grants.scratch_parent).unwrap().count(), 0);
    assert_eq!(
        fs::read_to_string(origin.join("probe.py")).unwrap(),
        "def value():\n    return 99\n"
    );
}
