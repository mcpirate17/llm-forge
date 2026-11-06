//! Authored candidate capture controls and a guest-free matched microbenchmark.
use super::*;
use crate::{
    candidate_execution_plan::{SelectionArgs, Surface},
    candidate_plan::Profile,
    candidate_snapshot::Limits,
    candidate_source::Kind,
};
use serde_json::{json, Value};
use std::{
    ffi::CString,
    fs::{self, File},
    io::Read,
    os::{fd::FromRawFd, unix::ffi::OsStrExt, unix::fs::PermissionsExt},
    time::Instant,
};

const POLICY: &str = "schema_version=1\nblock_at='high'\nmax_workers=2\ncache_ttl_days=7\nclaim_max_age_hours=24\nmax_file_bytes=1048576\nmax_binary_bytes=67108864\ncoverage_threshold=80.0\nhigh_risk_coverage_threshold=90.0\nbaseline_expires=2030-02-28\n[classes]\npython=['pkg/*.py']\ntest=['tests/*.py']\n[risk]\nhigh=[]\n[paths]\nprotected_deletes=[]\nhot=[]\ngenerated=[]\n[checks.equivalence-probe]\nkind='builtin'\nprofiles=['fast']\nalways=true\n";
const CONFIG: &str =
    "[tool.forge.equivalence_probe]\nper_module_timeout=12.5\njobs=2\nwaivers='waivers.json'\n";
const BASE: &str = "def changed(x):\n    return x + 1\n\n\ndef untouched(x):\n    return x * 2\n";
const CANDIDATE: &str =
    "def changed(x):\n    return x + 2\n\n\ndef untouched(x):\n    return x * 2\n";
const TEST: &str =
    "from pkg.target import changed\n\ndef test_changed():\n    assert changed(1) == 3\n";

struct Fixture {
    selection: Selection,
    snapshot: Snapshot,
    root: tempfile::TempDir,
}
impl Fixture {
    fn new(auxiliary_files: usize) -> Self {
        Self::with_record(auxiliary_files, false)
    }
    fn with_record(auxiliary_files: usize, record: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let repo_path = root.path().join("repo");
        let repository = git2::Repository::init(&repo_path).unwrap();
        for name in ["scratch", "cgroup"] {
            let path = root.path().join(name);
            fs::create_dir(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        for (name, data) in [
            ("policy.toml", POLICY),
            ("pyproject.toml", CONFIG),
            ("waivers.json", "[]\n"),
            ("pkg/target.py", BASE),
            ("tests/test_target.py", TEST),
        ] {
            stage(&repository, name, data.as_bytes());
        }
        let data = vec![b'x'; 1024];
        for number in 0..auxiliary_files {
            stage(&repository, &format!("data/{number:04}.dat"), &data);
        }
        if record {
            let path = root.path().join("granted-record.sqlite");
            fs::write(&path, b"malformed SQLite fixture\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let config = format!(
                "{CONFIG}\n[tool.forge.test_record]\nstore='{}'\n",
                path.display()
            );
            stage(&repository, "pyproject.toml", config.as_bytes());
        }
        let tree_id = repository.index().unwrap().write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = git2::Signature::new(
            "fixture",
            "fixture@example.invalid",
            &git2::Time::new(1_700_000_000, 0),
        )
        .unwrap();
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "authored baseline",
                &tree,
                &[],
            )
            .unwrap();
        stage(&repository, "pkg/target.py", CANDIDATE.as_bytes());
        let grants_path = root.path().join("grants.json");
        let grants = json!({"version":1,"source_root":repo_path,"read_only":[repo_path],
            "programs":{},"environment":{},"scratch_parent":root.path().join("scratch"),
            "cgroup_parent":root.path().join("cgroup"),"python":null,
            "resources":{"cpu_millis":1000,"memory_mib":128,"processes":16},
            "max_wall_seconds":30,"max_cpu_seconds":30,"max_address_space_mib":512});
        fs::write(&grants_path, serde_json::to_vec(&grants).unwrap()).unwrap();
        fs::set_permissions(&grants_path, fs::Permissions::from_mode(0o600)).unwrap();
        let selection = Selection::load(&SelectionArgs {
            repo: repo_path,
            kind: Kind::Index,
            target_ref: "HEAD".into(),
            base_ref: None,
            policy: Some("policy.toml".into()),
            profile: Some(Profile::Fast),
            today: Some("2030-01-10".into()),
            grants: grants_path,
            expected_tree: None,
            surface: Some(Surface::Manual),
        })
        .unwrap();
        let source = selection.source().unwrap();
        let snapshot = Snapshot::create(
            &source,
            &selection.candidate.tree_oid,
            &root.path().join("snapshot"),
            Limits::default(),
        )
        .unwrap();
        let fixture = Self {
            selection,
            snapshot,
            root,
        };
        fixture.assert_clean();
        fixture
    }
    fn dispose(self) {
        self.assert_clean();
        let Self {
            selection,
            snapshot,
            root,
        } = self;
        drop(selection);
        let path = snapshot.path();
        snapshot.discard().unwrap();
        assert!(!path.exists(), "review snapshot was not disposed");
        root.close().unwrap();
    }
    fn scratch(&self) -> PathBuf {
        self.root.path().join("scratch")
    }
    fn assert_clean(&self) {
        assert_eq!(fs::read_dir(self.scratch()).unwrap().count(), 0);
    }
    fn capture(&self) -> Captured<'_> {
        capture(&self.selection, &self.snapshot, None, None, 1).unwrap()
    }
    fn identity(&self) -> Value {
        let files = [
            "pyproject.toml",
            "waivers.json",
            "pkg/target.py",
            "tests/test_target.py",
        ]
        .map(|name| {
            let bytes = fs::read(self.snapshot.path().join(name)).unwrap();
            (name, format!("{:x}", Sha256::digest(bytes)))
        });
        json!({"candidate_tree":self.selection.candidate.tree_oid,
            "base_tree":self.selection.candidate.base_tree_oid,
            "policy_sha256":self.selection.policy.digest,
            "files":files})
    }
}
fn stage(repository: &git2::Repository, name: &str, bytes: &[u8]) {
    let path = repository.workdir().unwrap().join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(Path::new(name)).unwrap();
    index.write().unwrap();
}
fn expected_targets() -> Vec<slop_core::gate_types::Target> {
    vec![(
        "pkg/target.py".into(),
        false,
        vec![("changed".into(), 1, vec![])],
        vec![],
    )]
}
fn assert_capture(fixture: &Fixture, captured: &Captured<'_>) {
    assert_eq!(captured.targets, expected_targets());
    assert_eq!(captured.timeout_seconds, 12.5);
    assert_eq!(captured.jobs, 1);
    assert_eq!(captured.waivers.as_deref(), Some(b"[]\n".as_slice()));
    assert_eq!(
        captured.evidence,
        json!({"candidate_tree":fixture.selection.candidate.tree_oid,
        "changed":[["pkg/target.py",[2]]],"target_count":1,"record_sha256":null,
        "waivers_sha256":format!("{:x}",Sha256::digest(b"[]\n")),"execution_verified":false})
    );
    captured.verify().unwrap();
    fixture.assert_clean();
}

/// Observe actual scratch child creation/removal independently of capture internals.
struct ScratchWatch(File);
impl ScratchWatch {
    fn new(path: &Path) -> Self {
        // SAFETY: flags are valid and the returned descriptor is owned exactly once.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        assert!(fd >= 0, "inotify init: {}", std::io::Error::last_os_error());
        let file = unsafe { File::from_raw_fd(fd) };
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let mask = libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO;
        // SAFETY: path is a valid NUL-terminated string and fd remains owned by file.
        assert!(
            unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) } >= 0,
            "inotify watch: {}",
            std::io::Error::last_os_error()
        );
        Self(file)
    }
    fn drain(&mut self) -> Vec<Value> {
        let mut events = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let count = match self.0.read(&mut buffer) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("inotify read: {error}"),
            };
            assert!(count > 0, "inotify unexpectedly closed");
            let mut offset = 0;
            while offset < count {
                let size = std::mem::size_of::<libc::inotify_event>();
                assert!(count - offset >= size);
                // SAFETY: a complete header is present; the byte array need not be aligned.
                let event = unsafe {
                    std::ptr::read_unaligned(
                        buffer.as_ptr().add(offset).cast::<libc::inotify_event>(),
                    )
                };
                assert_eq!(event.mask & libc::IN_Q_OVERFLOW, 0, "lost scratch events");
                let end = offset + size + event.len as usize;
                assert!(end <= count);
                let name = &buffer[offset + size..end];
                let name = &name[..name.iter().position(|b| *b == 0).unwrap_or(name.len())];
                events.push(json!({"mask":event.mask,"name":String::from_utf8_lossy(name)}));
                offset = end;
            }
        }
        events
    }
}

#[test]
fn no_record_capture_preserves_exact_changed_unit_plan_and_input_identity() {
    let fixture = Fixture::new(0);
    let identity = fixture.identity();
    let captured = fixture.capture();
    assert_capture(&fixture, &captured);
    assert_eq!(fixture.identity(), identity);
    drop(captured);
    fixture.dispose();
}

#[test]
fn held_python_config_and_waiver_drift_are_refused() {
    for name in [
        "pkg/target.py",
        "tests/test_target.py",
        "pyproject.toml",
        "waivers.json",
    ] {
        let fixture = Fixture::new(0);
        let captured = fixture.capture();
        assert_capture(&fixture, &captured);
        fs::write(
            fixture.snapshot.path().join(name),
            b"changed after capture\n",
        )
        .unwrap();
        assert!(
            captured.verify().is_err(),
            "accepted held-input drift in {name}"
        );
        fixture.assert_clean();
        drop(captured);
        fixture.dispose();
    }
}

#[test]
fn no_record_capture_does_not_create_temporary_workspace() {
    let fixture = Fixture::new(0);
    let mut watch = ScratchWatch::new(&fixture.scratch());
    let captured = fixture.capture();
    assert_capture(&fixture, &captured);
    assert!(
        watch.drain().is_empty(),
        "no-record capture created scratch children"
    );
    drop(captured);
    drop(watch);
    fixture.dispose();
}

#[test]
fn malformed_pinned_record_preserves_original_and_disposes_private_workspace() {
    // Error-path coverage only: this does not prove valid recorded-driver selection.
    let fixture = Fixture::with_record(0, true);
    let path = fixture.root.path().join("granted-record.sqlite");
    let original = fs::read(&path).unwrap();
    let digest = format!("{:x}", Sha256::digest(&original));
    let mut watch = ScratchWatch::new(&fixture.scratch());
    let error = capture(
        &fixture.selection,
        &fixture.snapshot,
        Some((&path, &digest)),
        None,
        1,
    )
    .err()
    .expect("malformed SQLite record was accepted");
    assert!(format!("{error:#}").contains("not a database"), "{error:#}");
    let events = watch.drain();
    for mask in [libc::IN_CREATE, libc::IN_DELETE] {
        assert!(
            events.iter().any(|event| {
                let observed = event["mask"].as_u64().unwrap() as u32;
                observed & (mask | libc::IN_ISDIR) == (mask | libc::IN_ISDIR)
            }),
            "record error path did not create/dispose private scratch: {events:?}"
        );
    }
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&path).unwrap())),
        digest
    );
    fixture.assert_clean();
    drop(watch);
    fixture.dispose();
}

#[test]
#[ignore = "bounded guest-free capture benchmark; run original and optimized binaries with identical fixtures"]
fn benchmark_no_record_capture_small_and_large() {
    let executable = std::env::current_exe().unwrap();
    let executable_sha256 = format!("{:x}", Sha256::digest(fs::read(&executable).unwrap()));
    for auxiliary_files in [0, 1000] {
        let fixture = Fixture::new(auxiliary_files);
        let identity = fixture.identity();
        let mut watch = ScratchWatch::new(&fixture.scratch());
        let mut expected_evidence = None;
        for iteration in 0..12 {
            assert!(watch.drain().is_empty(), "unaccounted scratch activity");
            let start = Instant::now();
            let captured = fixture.capture();
            let elapsed_ns = start.elapsed().as_nanos();
            let events = watch.drain();
            assert_capture(&fixture, &captured);
            assert_eq!(fixture.identity(), identity);
            if let Some(expected) = &expected_evidence {
                assert_eq!(&captured.evidence, expected);
            } else {
                expected_evidence = Some(captured.evidence.clone());
            }
            println!(
                "{}",
                json!({"benchmark":"equivalence_capture_no_record_v1",
                "auxiliary_files":auxiliary_files,"auxiliary_bytes_each":1024,
                "phase":if iteration<2 {"warmup"} else {"measured"},
                "iteration":iteration,"elapsed_ns":elapsed_ns,"outcome":"captured_verified",
                "targets":captured.targets,"evidence":captured.evidence,"input_identity":identity,
                "scratch_events":events,"scratch_empty":true,
                "executable":executable,"executable_sha256":executable_sha256})
            );
        }
        drop(watch);
        fixture.dispose();
    }
}

/// A committed source fixture with real operator grants and no candidate policy.
struct CommittedFixture {
    source: Source,
    snapshot: Snapshot,
    grants: crate::candidate_execution_workspace::Grants,
    root: tempfile::TempDir,
}
impl CommittedFixture {
    fn new() -> Self {
        Self::authored(false, CANDIDATE)
    }
    fn authored(alias: bool, module: &str) -> Self {
        Self::configured(alias, module, CONFIG)
    }
    fn configured(alias: bool, module: &str, config: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let repo_path = root.path().join("repo");
        let repository = git2::Repository::init(&repo_path).unwrap();
        for (name, data) in [
            ("pyproject.toml", config),
            ("waivers.json", "[]\n"),
            ("pkg/target.py", module),
            ("tests/test_target.py", TEST),
            (".gitignore", "ignored.py\n"),
        ] {
            stage(&repository, name, data.as_bytes());
        }
        let mut index = repository.index().unwrap();
        if alias {
            std::os::unix::fs::symlink("target.py", repo_path.join("pkg/alias.py")).unwrap();
            index.add_path(Path::new("pkg/alias.py")).unwrap();
            index.write().unwrap();
        }
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
        let commit = repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "committed module",
                &tree,
                &[],
            )
            .unwrap();
        for name in ["scratch", "cgroup"] {
            let path = root.path().join(name);
            fs::create_dir(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let source = Source::open(&repo_path).unwrap();
        let tree = source.commit_tree(&commit.to_string()).unwrap();
        let snapshot = Snapshot::create(
            &source,
            &tree,
            &root.path().join("snapshot"),
            Limits::default(),
        )
        .unwrap();
        let path = root.path().join("grants.json");
        let document = json!({"version":1,"source_root":repo_path,"read_only":[repo_path],
            "programs":{},"environment":{},"scratch_parent":root.path().join("scratch"),
            "cgroup_parent":root.path().join("cgroup"),"python":null,
            "resources":{"cpu_millis":1000,"memory_mib":128,"processes":16},
            "max_wall_seconds":30,"max_cpu_seconds":30,"max_address_space_mib":512});
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let grants = crate::candidate_execution_workspace::Grants::load(&path, &source).unwrap();
        Self {
            source,
            snapshot,
            grants,
            root,
        }
    }
    fn capture(&self, module: &str) -> Result<Captured<'_>> {
        capture_committed(
            CaptureContext {
                origin: &self.source,
                tree_source: &self.source,
                snapshot: &self.snapshot,
                grants: &self.grants,
            },
            module,
            None,
            None,
            1,
        )
    }
    fn assert_clean(&self) {
        assert_eq!(
            fs::read_dir(self.root.path().join("scratch"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn committed_whole_module_uses_committed_inputs_despite_dirty_host() {
    let fixture = CommittedFixture::new();
    let repository = git2::Repository::open(fixture.source.root()).unwrap();
    stage(&repository, "pkg/target.py", b"staged = True\n");
    for (name, bytes) in [
        ("pkg/target.py", "dirty = True\n"),
        ("pyproject.toml", "broken = [\n"),
        ("waivers.json", "dirty waivers\n"),
        ("tests/test_target.py", "invalid test\n"),
        ("untracked.py", "untracked = True\n"),
        ("ignored.py", "ignored = True\n"),
    ] {
        fs::write(fixture.source.root().join(name), bytes).unwrap();
    }
    let index = fs::read(fixture.source.root().join(".git/index")).unwrap();
    let captured = fixture.capture("pkg/target.py").unwrap();
    assert_eq!(
        captured.targets,
        vec![(
            "pkg/target.py".into(),
            true,
            vec![
                ("changed".into(), 1, vec![]),
                ("untouched".into(), 5, vec![])
            ],
            vec![]
        )]
    );
    assert_eq!(captured.waivers.as_deref(), Some(b"[]\n".as_slice()));
    assert_eq!(captured.timeout_seconds, 12.5);
    assert_eq!(captured.jobs, 1);
    assert_eq!(captured.evidence["source_kind"], "commit_tree");
    assert_eq!(captured.evidence["scope"], "whole_module");
    assert!(captured.evidence["record_provenance"].is_null());
    assert!(!fixture.snapshot.path().join("untracked.py").exists());
    assert!(!fixture.snapshot.path().join("ignored.py").exists());
    assert!(!fixture.snapshot.path().join("policy.toml").exists());
    captured.verify().unwrap();
    assert_eq!(
        fs::read(fixture.source.root().join(".git/index")).unwrap(),
        index
    );
    assert_eq!(
        fs::read_to_string(fixture.source.root().join("pkg/target.py")).unwrap(),
        "dirty = True\n"
    );
    fixture.assert_clean();
}

#[test]
fn committed_module_refusals_and_snapshot_input_drift_fail_closed() {
    let linked = CommittedFixture::authored(true, CANDIDATE);
    assert!(linked.capture("pkg/alias.py").is_err());
    linked.assert_clean();
    let fixture = CommittedFixture::new();
    for module in [
        "../target.py",
        "/pkg/target.py",
        "pkg//target.py",
        "pkg/*.py",
        "pkg/alias.py",
        "pkg/missing.py",
        "pyproject.toml",
    ] {
        assert!(fixture.capture(module).is_err(), "accepted {module}");
    }
    fixture.assert_clean();
    for name in [
        "pkg/target.py",
        "tests/test_target.py",
        "pyproject.toml",
        "waivers.json",
    ] {
        let fixture = CommittedFixture::new();
        let captured = fixture.capture("pkg/target.py").unwrap();
        captured.verify().unwrap();
        fs::write(fixture.snapshot.path().join(name), b"drift\n").unwrap();
        assert!(
            captured.verify().is_err(),
            "accepted committed input drift: {name}"
        );
        fixture.assert_clean();
    }
}

#[test]
fn distinct_committed_module_bytes_produce_distinct_bound_whole_plans() {
    let first = CommittedFixture::new();
    let second = CommittedFixture::authored(false, "def other(x):\n    return x + 7\n");
    let one = first.capture("pkg/target.py").unwrap();
    let two = second.capture("pkg/target.py").unwrap();
    assert_ne!(
        first.snapshot.report().tree_oid,
        second.snapshot.report().tree_oid
    );
    assert_ne!(
        one.evidence["candidate_tree"],
        two.evidence["candidate_tree"]
    );
    assert_ne!(one.targets, two.targets);
    assert_eq!(
        two.targets,
        vec![(
            "pkg/target.py".into(),
            true,
            vec![("other".into(), 1, vec![])],
            vec![]
        )]
    );
    one.verify().unwrap();
    two.verify().unwrap();
    first.assert_clean();
    second.assert_clean();
}

#[test]
fn committed_capture_refuses_unsolicited_external_record_grant() {
    let fixture = CommittedFixture::new();
    let path = fixture.root.path().join("record.sqlite");
    fs::write(&path, b"unconfigured record").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let hash = format!("{:x}", Sha256::digest(fs::read(&path).unwrap()));
    let result = capture_committed(
        CaptureContext {
            origin: &fixture.source,
            tree_source: &fixture.source,
            snapshot: &fixture.snapshot,
            grants: &fixture.grants,
        },
        "pkg/target.py",
        Some((&path, &hash)),
        None,
        1,
    );
    let error = result.err().expect("unsolicited record grant was accepted");
    assert!(format!("{error:#}").contains("without enabled captured configuration"));
    assert_eq!(fs::read(&path).unwrap(), b"unconfigured record");
    fixture.assert_clean();
}

/// Different original/private Git roots, with grants loaded only for the origin.
struct OriginFixture {
    origin: Source,
    grants: crate::candidate_execution_workspace::Grants,
    private: CommittedFixture,
}
impl OriginFixture {
    fn new(config: &str) -> Self {
        let private = CommittedFixture::configured(false, CANDIDATE, config);
        let repository = git2::Repository::init(private.root.path().join("origin")).unwrap();
        stage(
            &repository,
            "pyproject.toml",
            b"origin config is deliberately invalid [\n",
        );
        stage(
            &repository,
            "pkg/target.py",
            b"def origin_only():\n    return 0\n",
        );
        let tree = repository
            .find_tree(repository.index().unwrap().write_tree().unwrap())
            .unwrap();
        let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "real origin",
                &tree,
                &[],
            )
            .unwrap();
        let origin = Source::open(repository.workdir().unwrap()).unwrap();
        let mut document: Value =
            serde_json::from_slice(&fs::read(private.root.path().join("grants.json")).unwrap())
                .unwrap();
        document["source_root"] = json!(origin.root());
        document["read_only"] = json!([origin.root()]);
        let path = private.root.path().join("origin-grants.json");
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let grants = crate::candidate_execution_workspace::Grants::load(&path, &origin).unwrap();
        for root in [origin.root(), private.source.root()] {
            for name in ["out", "records"] {
                let directory = root.join(name);
                fs::create_dir(&directory).unwrap();
                fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        Self {
            origin,
            grants,
            private,
        }
    }
    fn context(&self) -> CaptureContext<'_, '_> {
        CaptureContext {
            origin: &self.origin,
            tree_source: &self.private.source,
            snapshot: &self.private.snapshot,
            grants: &self.grants,
        }
    }
    fn configure_private(&mut self, config: &str) {
        let repository = git2::Repository::open(self.private.source.root()).unwrap();
        stage(&repository, "pyproject.toml", config.as_bytes());
        let parent = repository.head().unwrap().peel_to_commit().unwrap();
        let tree = repository
            .find_tree(repository.index().unwrap().write_tree().unwrap())
            .unwrap();
        let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
        let commit = repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "captured external configuration",
                &tree,
                &[&parent],
            )
            .unwrap();
        let tree = self
            .private
            .source
            .commit_tree(&commit.to_string())
            .unwrap();
        let snapshot = Snapshot::create(
            &self.private.source,
            &tree,
            &self.private.root.path().join("replacement-snapshot"),
            Limits::default(),
        )
        .unwrap();
        std::mem::replace(&mut self.private.snapshot, snapshot)
            .discard()
            .unwrap();
    }
    fn record(&self, root: &Path, bytes: &[u8]) -> (PathBuf, String) {
        let path = root.join("records/current.sqlite");
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        (path, format!("{:x}", Sha256::digest(bytes)))
    }
}

#[test]
fn split_origin_publishes_relative_artifacts_at_origin_using_private_captured_config() {
    let fixture = OriginFixture::new(&format!("{CONFIG}run_artifacts='out'\n"));
    let output = fixture.origin.root().join("out");
    let private_output = fixture.private.source.root().join("out");
    let mut watch = ScratchWatch::new(&fixture.grants.scratch_parent);
    let captured =
        capture_committed(fixture.context(), "pkg/target.py", None, Some(&output), 1).unwrap();
    assert_eq!(
        captured.targets[0]
            .2
            .iter()
            .map(|unit| unit.0.as_str())
            .collect::<Vec<_>>(),
        ["changed", "untouched"]
    );
    assert_eq!(captured.timeout_seconds, 12.5);
    assert_eq!(captured.waivers.as_deref(), Some(b"[]\n".as_slice()));
    captured.verify().unwrap();
    let published = captured
        .artifacts
        .as_ref()
        .unwrap()
        .publish(b"{}", true)
        .unwrap();
    assert_eq!(published.parent(), Some(output.as_path()));
    assert_eq!(fs::read(published).unwrap(), b"{}");
    assert_eq!(fs::read_dir(private_output).unwrap().count(), 0);
    assert!(
        watch.drain().is_empty(),
        "split no-record capture created a Workspace"
    );
    assert!(capture_committed(
        fixture.context(),
        "pkg/target.py",
        None,
        Some(&fixture.private.source.root().join("out")),
        1
    )
    .is_err());
    fixture.private.assert_clean();
}

#[test]
fn split_origin_relative_record_admission_pins_origin_bytes_and_rejects_private_substitute() {
    // Tests record admission only, without claiming these bytes form a valid SQLite driver DB.
    let fixture = OriginFixture::new(&format!(
        "{CONFIG}\n[tool.forge.test_record]\nstore='records/current.sqlite'\n"
    ));
    let (original, hash) = fixture.record(fixture.origin.root(), b"origin current data");
    let (substitute, substitute_hash) =
        fixture.record(fixture.private.source.root(), b"private substitute");
    let document: toml::Value = toml::from_str(
        &fs::read_to_string(fixture.private.snapshot.path().join("pyproject.toml")).unwrap(),
    )
    .unwrap();
    let configured = || record_config(&document).unwrap();
    let context = fixture.context();
    context.validate().unwrap();
    let (held, bytes, path) =
        capture_record(configured(), Some((&original, &hash)), &context).unwrap();
    assert_eq!(bytes.as_deref(), Some(b"origin current data".as_slice()));
    assert_eq!(path, Some(original.clone()));
    held.unwrap().verify().unwrap();
    assert!(capture_record(
        configured(),
        Some((&substitute, &substitute_hash)),
        &context
    )
    .is_err());
    assert_eq!(fs::read(original).unwrap(), b"origin current data");
    assert_eq!(fs::read(substitute).unwrap(), b"private substitute");
    fixture.private.assert_clean();
}

#[test]
fn split_origin_rejects_private_grant_and_scratch_record_or_output_substitution() {
    let fixture = OriginFixture::new(&format!("{CONFIG}run_artifacts='out'\n"));
    let incorrect = CaptureContext {
        origin: &fixture.origin,
        tree_source: &fixture.private.source,
        snapshot: &fixture.private.snapshot,
        grants: &fixture.private.grants,
    };
    assert!(capture_committed(incorrect, "pkg/target.py", None, None, 1).is_err());
    let scratch = &fixture.grants.scratch_parent;
    let scratch_output = scratch.join("out");
    fs::create_dir(&scratch_output).unwrap();
    fs::set_permissions(&scratch_output, fs::Permissions::from_mode(0o700)).unwrap();
    let origin_output = fixture.origin.root().join("out");
    fs::remove_dir(&origin_output).unwrap();
    std::os::unix::fs::symlink(&scratch_output, &origin_output).unwrap();
    assert!(capture_committed(
        fixture.context(),
        "pkg/target.py",
        None,
        Some(&scratch_output),
        1
    )
    .is_err());
    let record = scratch.join("record.sqlite");
    fs::write(&record, b"scratch substitute").unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(
        &record,
        fixture.origin.root().join("records/current.sqlite"),
    )
    .unwrap();
    let document: toml::Value =
        toml::from_str("[tool.forge.test_record]\nstore='records/current.sqlite'\n").unwrap();
    let hash = format!("{:x}", Sha256::digest(fs::read(&record).unwrap()));
    assert!(capture_record(
        record_config(&document).unwrap(),
        Some((&record, &hash)),
        &fixture.context()
    )
    .is_err());
    assert_eq!(fs::read(&record).unwrap(), b"scratch substitute");
    // These are authored test inputs, removed explicitly before asserting no Workspace leak.
    fs::remove_file(record).unwrap();
    fs::remove_dir(scratch_output).unwrap();
    fixture.private.assert_clean();
}

#[test]
fn split_origin_captured_absolute_paths_cannot_authorize_private_source_outputs_or_records() {
    let mut fixture = OriginFixture::new(CONFIG);
    let output = fixture.private.source.root().join("out");
    fixture.configure_private(&format!("{CONFIG}run_artifacts='{}'\n", output.display()));
    let error = capture_committed(fixture.context(), "pkg/target.py", None, Some(&output), 1)
        .err()
        .expect("private source output admitted");
    assert!(
        format!("{error:#}").contains("excluded snapshot or scratch"),
        "{error:#}"
    );
    assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
    let (record, hash) = fixture.record(fixture.private.source.root(), b"private record");
    fixture.configure_private(&format!(
        "{CONFIG}\n[tool.forge.test_record]\nstore='{}'\n",
        record.display()
    ));
    let error = capture_committed(
        fixture.context(),
        "pkg/target.py",
        Some((&record, &hash)),
        None,
        1,
    )
    .err()
    .expect("private source record admitted");
    assert!(
        format!("{error:#}").contains("excluded snapshot or scratch"),
        "{error:#}"
    );
    assert_eq!(fs::read(record).unwrap(), b"private record");
    fixture.private.assert_clean();
}

#[test]
fn same_root_record_admission_preserves_owned_scratch_compatibility_but_split_origin_refuses() {
    // Admission-only control: these bytes are not evidence of a valid SQLite driver DB.
    let original = CommittedFixture::new();
    let private = CommittedFixture::new();
    let path = original
        .grants
        .scratch_parent
        .join("operator-record.sqlite");
    fs::write(&path, b"operator pinned current data").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let hash = format!("{:x}", Sha256::digest(fs::read(&path).unwrap()));
    let document: toml::Value = toml::from_str(&format!(
        "[tool.forge.test_record]\nstore='{}'\n",
        path.display()
    ))
    .unwrap();
    let legacy = CaptureContext {
        origin: &original.source,
        tree_source: &original.source,
        snapshot: &original.snapshot,
        grants: &original.grants,
    };
    legacy.validate().unwrap();
    let (held, bytes, captured_path) = capture_record(
        record_config(&document).unwrap(),
        Some((&path, &hash)),
        &legacy,
    )
    .unwrap();
    assert_eq!(
        bytes.as_deref(),
        Some(b"operator pinned current data".as_slice())
    );
    assert_eq!(captured_path.as_deref(), Some(path.as_path()));
    held.unwrap().verify().unwrap();
    let split = CaptureContext {
        origin: &original.source,
        tree_source: &private.source,
        snapshot: &private.snapshot,
        grants: &original.grants,
    };
    split.validate().unwrap();
    assert!(capture_record(
        record_config(&document).unwrap(),
        Some((&path, &hash)),
        &split
    )
    .is_err());
    assert_eq!(fs::read(&path).unwrap(), b"operator pinned current data");
    fs::remove_file(path).unwrap();
    original.assert_clean();
    private.assert_clean();
}
