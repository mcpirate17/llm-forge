//! Guest-free whole-module selection and empty-selection configuration controls.
use super::*;
use crate::candidate_snapshot::Limits;
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt};

const CONFIG: &str =
    "[tool.forge.equivalence_probe]\nper_module_timeout=7.5\njobs=3\nwaivers='waivers.json'\n";
const MODULES: [&str; 2] = ["pkg/a.py", "pkg/b.py"];

struct Fixture {
    source: Source,
    grants: Grants,
    tree: String,
    root: tempfile::TempDir,
}
impl Fixture {
    fn new(config: Option<&[u8]>) -> Self {
        Self::authored(config, false)
    }
    fn authored(config: Option<&[u8]>, alias: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        private_dir(root.path());
        let repository = git2::Repository::init(root.path().join("repo")).unwrap();
        for (name, bytes) in [
            ("pkg/a.py", b"def alpha(x):\n    return x + 1\n".as_slice()),
            ("pkg/b.py", b"def beta(x):\n    return x * 2\n".as_slice()),
            (
                "tests/test_a.py",
                b"def test_alpha():\n    pass\n".as_slice(),
            ),
            ("waivers.json", b"[]\n".as_slice()),
            ("data.txt", b"non Python\n".as_slice()),
        ] {
            stage(&repository, name, bytes);
        }
        if let Some(bytes) = config {
            stage(&repository, "pyproject.toml", bytes);
        }
        let mut index = repository.index().unwrap();
        if alias {
            std::os::unix::fs::symlink("a.py", repository.workdir().unwrap().join("pkg/link.py"))
                .unwrap();
            index.add_path(Path::new("pkg/link.py")).unwrap();
            index.write().unwrap();
        }
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "authored modules",
                &tree,
                &[],
            )
            .unwrap();
        let tree = tree.id().to_string();
        let source = Source::open(repository.workdir().unwrap()).unwrap();
        for name in ["scratch", "cgroup", "out"] {
            private_dir(&root.path().join(name));
        }
        let path = root.path().join("grants.json");
        let document = json!({"version":1,"source_root":source.root(),
            "read_only":[source.root()],"programs":{},"environment":{},"python":null,
            "scratch_parent":root.path().join("scratch"),
            "cgroup_parent":root.path().join("cgroup"),
            "resources":{"cpu_millis":1000,"memory_mib":128,"processes":16},
            "max_wall_seconds":30,"max_cpu_seconds":30,"max_address_space_mib":512});
        private_file(&path, &serde_json::to_vec(&document).unwrap());
        let grants = Grants::load(&path, &source).unwrap();
        Self {
            source,
            grants,
            tree,
            root,
        }
    }
    fn snapshot(&self) -> Snapshot {
        Snapshot::create(
            &self.source,
            &self.tree,
            &self.root.path().join("snapshot"),
            Limits::default(),
        )
        .unwrap()
    }
    fn context<'a>(&'a self, snapshot: &'a Snapshot) -> CaptureContext<'a, 'a> {
        CaptureContext {
            origin: &self.source,
            tree_source: &self.source,
            snapshot,
            grants: &self.grants,
        }
    }
    fn empty(&self, supplied: Option<&Path>, jobs: usize) -> Result<Option<Artifacts>> {
        empty_artifacts(
            &self.source,
            &self.source,
            &self.tree,
            &self.grants,
            supplied,
            jobs,
        )
    }
    fn assert_clean(&self) {
        assert_eq!(
            fs::read_dir(&self.grants.scratch_parent).unwrap().count(),
            0
        );
    }
    fn dispose(&self, snapshot: Snapshot) {
        self.assert_clean();
        let path = snapshot.path();
        snapshot.discard().unwrap();
        assert!(!path.exists());
        self.assert_clean();
    }
}
fn private_dir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn stage(repository: &git2::Repository, name: &str, bytes: &[u8]) {
    let path = repository.workdir().unwrap().join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(Path::new(name)).unwrap();
    index.write().unwrap();
}
fn names(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

#[test]
fn multiple_modules_share_one_captured_config_and_keep_whole_module_targets() {
    let fixture = Fixture::new(Some(CONFIG.as_bytes()));
    let snapshot = fixture.snapshot();
    // Dirty host content must not change either module or the shared config.
    fs::write(fixture.source.root().join("pkg/a.py"), "invalid [").unwrap();
    fs::write(fixture.source.root().join("pyproject.toml"), "invalid [").unwrap();
    let captured =
        capture_modules(fixture.context(&snapshot), &names(&MODULES), None, None, 2).unwrap();
    assert_eq!(
        captured.targets,
        vec![
            (
                "pkg/a.py".into(),
                true,
                vec![("alpha".into(), 1, vec![])],
                vec![]
            ),
            (
                "pkg/b.py".into(),
                true,
                vec![("beta".into(), 1, vec![])],
                vec![]
            ),
        ]
    );
    assert_eq!(captured.timeout_seconds, 7.5);
    assert_eq!(captured.jobs, 2);
    assert_eq!(captured.waivers.as_deref(), Some(b"[]\n".as_slice()));
    assert_eq!(
        captured.evidence,
        json!({"candidate_tree":fixture.tree,
        "changed":[["pkg/a.py",null],["pkg/b.py",null]],"target_count":2,
        "record_sha256":null,"record_provenance":null,
        "waivers_sha256":format!("{:x}",Sha256::digest(b"[]\n")),
        "execution_verified":false,"source_kind":"commit_tree", "scope":"whole_modules",
        "modules":MODULES})
    );
    captured.verify().unwrap();
    fixture.assert_clean();
    // The one shared waiver binding protects the complete multi-module plan.
    fs::write(snapshot.path().join("waivers.json"), b"[{}]\n").unwrap();
    assert!(captured.verify().is_err());
    drop(captured);
    fixture.dispose(snapshot);
}

#[test]
fn invalid_multi_module_selections_refuse_before_leaving_scratch() {
    let fixture = Fixture::new(None);
    let snapshot = fixture.snapshot();
    for paths in [
        vec![],
        vec!["pkg/a.py", "pkg/a.py"],
        vec!["pkg/a.py", "missing.py"],
        vec!["../outside.py"],
        vec!["/absolute.py"],
        vec!["pkg/./a.py"],
        vec!["pkg/a.py\n"],
        vec!["data.txt"],
        vec!["pkg"],
    ] {
        let result = capture_modules(fixture.context(&snapshot), &names(&paths), None, None, 1);
        assert!(result.is_err(), "accepted {paths:?}");
        fixture.assert_clean();
    }
    // Refusal does not contaminate the held snapshot or a later valid plan.
    let captured =
        capture_modules(fixture.context(&snapshot), &names(&MODULES), None, None, 1).unwrap();
    assert_eq!(captured.targets.len(), 2);
    captured.verify().unwrap();
    drop(captured);
    fixture.dispose(snapshot);
}

#[test]
fn multi_module_selection_refuses_a_committed_symlink_without_touching_its_target() {
    let fixture = Fixture::authored(None, true);
    let snapshot = fixture.snapshot();
    let original = fs::read(fixture.source.root().join("pkg/a.py")).unwrap();
    assert!(capture_modules(
        fixture.context(&snapshot),
        &names(&["pkg/a.py", "pkg/link.py"]),
        None,
        None,
        1
    )
    .is_err());
    assert_eq!(
        fs::read(fixture.source.root().join("pkg/a.py")).unwrap(),
        original
    );
    fixture.dispose(snapshot);
}

#[test]
fn multi_module_record_configuration_needs_explicit_pins_and_preserves_error_input() {
    let fixture = Fixture::new(Some(
        b"[tool.forge.test_record]\nstore='../record.sqlite'\n",
    ));
    // An empty plan validates record schema but does not require or open a store.
    assert!(fixture.empty(None, 1).unwrap().is_none());
    let snapshot = fixture.snapshot();
    let path = fixture.root.path().join("record.sqlite");
    let bytes = b"deliberately malformed SQLite error-path fixture\n";
    private_file(&path, bytes);
    let hash = format!("{:x}", Sha256::digest(bytes));
    let missing = capture_modules(fixture.context(&snapshot), &names(&MODULES), None, None, 1)
        .err()
        .unwrap();
    assert!(format!("{missing:#}").contains("operator path and SHA-256"));
    let bad_pin = capture_modules(
        fixture.context(&snapshot),
        &names(&MODULES),
        Some((&path, &"0".repeat(64))),
        None,
        1,
    )
    .err()
    .unwrap();
    assert!(format!("{bad_pin:#}").contains("digest mismatch"));
    let malformed = capture_modules(
        fixture.context(&snapshot),
        &names(&MODULES),
        Some((&path, &hash)),
        None,
        1,
    );
    assert!(
        malformed.is_err(),
        "malformed record cannot produce a target plan"
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&path).unwrap())),
        hash
    );
    fixture.dispose(snapshot);
}

#[test]
fn empty_selection_accepts_absent_config_and_enforces_timeout_job_and_schema_limits() {
    let absent = Fixture::new(None);
    assert!(absent.empty(None, 1).unwrap().is_none());
    assert!(absent.empty(None, 0).is_err());
    assert!(absent.empty(None, 33).is_err());
    absent.assert_clean();
    for config in [
        "[tool.forge.equivalence_probe]\nper_module_timeout=0.0009\n",
        "[tool.forge.equivalence_probe]\nper_module_timeout=3600.1\n",
        "[tool.forge.equivalence_probe]\nper_module_timeout=nan\n",
        "[tool.forge.equivalence_probe]\njobs=0\n",
        "[tool.forge.equivalence_probe]\nunknown=true\n",
        "[tool.forge.equivalence_probe]\njobs='two'\n",
        "[tool.forge.test_record]\nstore='record'\nunknown=true\n",
        "[tool.forge.test_record]\nlines=true\n",
        "invalid [",
        "# nul\0\n",
    ] {
        let fixture = Fixture::new(Some(config.as_bytes()));
        assert!(fixture.empty(None, 2).is_err(), "accepted {config:?}");
        fixture.assert_clean();
    }
    for timeout in ["0.001", "3600.0"] {
        let config =
            format!("[tool.forge.equivalence_probe]\nper_module_timeout={timeout}\njobs=32\n");
        let fixture = Fixture::new(Some(config.as_bytes()));
        assert!(fixture.empty(None, 2).unwrap().is_none());
        fixture.assert_clean();
    }
}

#[test]
fn empty_selection_reads_bounded_regular_utf8_config_from_git_not_host() {
    let boundary = Fixture::new(Some(&vec![b'#'; 1024 * 1024]));
    assert!(boundary.empty(None, 1).unwrap().is_none());
    boundary.assert_clean();
    for bytes in [vec![b'#'; 1024 * 1024 + 1], vec![0xff]] {
        let fixture = Fixture::new(Some(&bytes));
        assert!(fixture.empty(None, 1).is_err());
        fixture.assert_clean();
    }
    let mut fixture = Fixture::new(Some(CONFIG.as_bytes()));
    fs::write(fixture.source.root().join("pyproject.toml"), "invalid [").unwrap();
    assert!(fixture.empty(None, 1).unwrap().is_none());
    let repository = fixture.source.repository();
    let original = repository
        .find_tree(git2::Oid::from_str(&fixture.tree).unwrap())
        .unwrap();
    let mut builder = repository.treebuilder(Some(&original)).unwrap();
    let target = repository.blob(b"outside-config").unwrap();
    builder.insert("pyproject.toml", target, 0o120000).unwrap();
    let tree = repository.find_tree(builder.write().unwrap()).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "authored config link",
            &tree,
            &[&parent],
        )
        .unwrap();
    fixture.tree = tree.id().to_string();
    assert!(fixture.empty(None, 1).is_err());
    fixture.assert_clean();
}

#[test]
fn empty_selection_artifacts_require_exact_config_grant_and_post_cleanup_publication() {
    let fixture = Fixture::new(Some(
        b"[tool.forge.equivalence_probe]\nrun_artifacts='../out'\n",
    ));
    // Use a canonical configured absolute directory; lexical parent paths are refused.
    let output = fixture.root.path().join("out");
    assert!(fixture.empty(Some(&output), 1).is_err());
    let config = format!(
        "[tool.forge.equivalence_probe]\nrun_artifacts='{}'\n",
        output.display()
    );
    let configured = Fixture::new(Some(config.as_bytes()));
    assert!(configured.empty(None, 1).is_err());
    assert!(configured.empty(Some(configured.source.root()), 1).is_err());
    let artifacts = configured.empty(Some(&output), 1).unwrap().unwrap();
    assert!(artifacts.publish(b"{}\n", false).is_err());
    assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
    let published = artifacts.publish(b"{}\n", true).unwrap();
    assert_eq!(fs::read(&published).unwrap(), b"{}\n");
    let absent = Fixture::new(None);
    assert!(absent.empty(Some(&output), 1).is_err());
    // Replacing an admitted output directory invalidates the held publication grant.
    fs::rename(&output, fixture.root.path().join("old-out")).unwrap();
    private_dir(&output);
    assert!(artifacts.publish(b"{}", true).is_err());
    assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
    configured.assert_clean();
}
