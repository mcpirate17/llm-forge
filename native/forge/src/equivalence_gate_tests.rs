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

    fn commit_paths(&mut self, paths: &[&str]) {
        let repo = self.source.repository();
        let mut index = repo.index().unwrap();
        for path in paths {
            index.add_path(std::path::Path::new(path)).unwrap();
        }
        index.write().unwrap();
        let tree = index.write_tree().unwrap();
        let signature = Signature::now("fixture", "fixture@example.invalid").unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        self.commit = repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "fixture inputs",
                &repo.find_tree(tree).unwrap(),
                &[&parent],
            )
            .unwrap()
            .to_string();
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
        selected.module = vec![module.into()];
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

fn automatic_args(base: Option<&str>) -> GateArgs {
    let mut requested = args();
    requested.commit = None;
    requested.live = true;
    requested.tracked_only = true;
    requested.module.clear();
    requested.changed_from = base.map(str::to_owned);
    requested.all_tracked = base.is_none();
    requested
}

#[test]
fn parser_preserves_repeated_modules_and_requires_one_selection_and_live_scope() {
    let common = [
        "gate",
        "--grants",
        "/operator/grants.json",
        "--guest-path",
        "/operator/guest.so",
        "--guest-sha256",
        "not-loaded",
    ];
    for extra in [
        vec!["--live", "--tracked-only"],
        vec![
            "--live",
            "--tracked-only",
            "--input",
            "src",
            "--all-tracked",
        ],
        vec![
            "--live",
            "--tracked-only",
            "--module",
            "a.py",
            "--all-tracked",
        ],
        vec![
            "--live",
            "--tracked-only",
            "--changed-from",
            "HEAD",
            "--all-tracked",
        ],
        vec![
            "--commit",
            "0123456789012345678901234567890123456789",
            "--all-tracked",
        ],
    ] {
        assert!(Command::try_parse_from(common.iter().copied().chain(extra)).is_err());
    }
    for extra in [
        vec![
            "--live",
            "--tracked-only",
            "--module",
            "b.py",
            "--module",
            "a.py",
        ],
        vec!["--live", "--input", "src", "--changed-from", "HEAD"],
        vec!["--live", "--tracked-only", "--all-tracked"],
    ] {
        let selected = Command::try_parse_from(common.iter().copied().chain(extra))
            .unwrap()
            .gate;
        validate_selection(&selected).unwrap();
        if !selected.module.is_empty() {
            assert_eq!(selected.module, ["b.py", "a.py"]);
        }
    }
    let mut selected = args();
    selected.module.push(selected.module[0].clone());
    assert!(validate_selection(&selected)
        .unwrap_err()
        .to_string()
        .contains("repeated"));
}

#[test]
fn automatic_selection_filters_tests_untracked_and_missing_files_and_binds_drift() {
    for changed in [false, true] {
        let mut f = Fixture::new();
        let root = f.source.root().to_owned();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::create_dir_all(root.join("test")).unwrap();
        let tracked = [
            "b.py",
            "a.py",
            "deleted.py",
            "tests/helper.py",
            "test/util.py",
            "test_root.py",
            "conftest.py",
            "notes.txt",
        ];
        for path in tracked {
            fs::write(root.join(path), "value = 1\n").unwrap();
        }
        f.commit_paths(&tracked);
        for path in tracked {
            fs::write(root.join(path), "value = 2\n").unwrap();
        }
        fs::remove_file(root.join("deleted.py")).unwrap();
        fs::write(root.join("untracked.py"), "value = 3\n").unwrap();
        let requested = automatic_args(changed.then_some(f.commit.as_str()));
        let selected = SelectedSource::capture(&requested, f.source, &f.grants).unwrap();
        let modules = SelectedModules::capture(&requested, &selected).unwrap();
        assert_eq!(modules.paths(), ["a.py", "b.py"]);
        assert!(modules.module().is_none());
        assert_eq!(modules.evidence()["module_count"], 2);
        modules.verify(&selected).unwrap();
        fs::write(root.join("a.py"), "value = 99\n").unwrap();
        assert!(modules.verify(&selected).is_err());
        selected.discard().unwrap();
        assert_eq!(fs::read_dir(&f.grants.scratch_parent).unwrap().count(), 0);
    }
}

#[test]
fn explicit_modules_keep_order_and_test_module_compatibility() {
    let f = Fixture::new();
    let mut requested = args();
    requested.commit = Some(f.commit.clone());
    requested.module = vec!["test_b.py".into(), "a.py".into()];
    let selected = SelectedSource::capture(&requested, f.source, &f.grants).unwrap();
    let modules = SelectedModules::capture(&requested, &selected).unwrap();
    assert_eq!(modules.paths(), ["test_b.py", "a.py"]);
    assert_eq!(selected.scope(), "committed-whole-modules");
    assert_eq!(modules.evidence()["mode"], "explicit");
    selected.discard().unwrap();
}

#[test]
fn automatic_symlink_modules_refuse_instead_of_silently_omitting_them() {
    for changed in [false, true] {
        let mut f = Fixture::new();
        let root = f.source.root().to_owned();
        fs::write(root.join("actual.py"), "value = 1\n").unwrap();
        f.commit_paths(&["actual.py"]);
        let base = f.commit.clone();
        std::os::unix::fs::symlink("actual.py", root.join("alias.py")).unwrap();
        f.commit_paths(&["alias.py"]);
        let requested = automatic_args(changed.then_some(base.as_str()));
        let selected = SelectedSource::capture(&requested, f.source, &f.grants).unwrap();
        let error = SelectedModules::capture(&requested, &selected)
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("ordinary Python module: alias.py"),
            "{error:#}"
        );
        selected.discard().unwrap();
        assert_eq!(fs::read_dir(&f.grants.scratch_parent).unwrap().count(), 0);
    }
}

#[test]
fn automatic_selection_refuses_dangling_and_escaping_module_links() {
    for target in ["absent.py", "../outside.py"] {
        let mut f = Fixture::new();
        let root = f.source.root().to_owned();
        std::os::unix::fs::symlink(target, root.join("alias.py")).unwrap();
        f.commit_paths(&["alias.py"]);
        let requested = automatic_args(None);
        match SelectedSource::capture(&requested, f.source, &f.grants) {
            Ok(selected) => {
                let error = SelectedModules::capture(&requested, &selected)
                    .err()
                    .unwrap();
                assert!(error
                    .to_string()
                    .contains("ordinary Python module: alias.py"));
                selected.discard().unwrap();
            }
            Err(error) => assert!(format!("{error:#}").contains("symlink"), "{error:#}"),
        }
        assert_eq!(fs::read_dir(&f.grants.scratch_parent).unwrap().count(), 0);
    }
}

#[test]
fn automatic_modes_keep_raw_snapshot_git_configuration_refusals() {
    for (key, value) in [
        ("core.symlinks", false),
        ("core.filemode", false),
        ("core.autocrlf", true),
    ] {
        let f = Fixture::new();
        f.source
            .repository()
            .config()
            .unwrap()
            .set_bool(key, value)
            .unwrap();
        let requested = automatic_args(None);
        let error = SelectedSource::capture(&requested, f.source, &f.grants)
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains(key), "{error:#}");
        assert_eq!(fs::read_dir(&f.grants.scratch_parent).unwrap().count(), 0);
    }
}
