use super::*;
use crate::candidate_snapshot::Limits;
use crate::candidate_source::Kind;
use std::{fs, path::PathBuf};

const POLICY: &str = r#"schema_version = 1
block_at = 'high'
max_workers = 2
cache_ttl_days = 7
claim_max_age_hours = 24
max_file_bytes = 100000
max_binary_bytes = 200000
coverage_threshold = 80.0
high_risk_coverage_threshold = 90.0
baseline_expires = 2026-11-30
[classes]
governance = ['conductor/*']
[risk]
high = ['conductor/candidate_review/*']
[paths]
protected_deletes = []
hot = []
generated = ['generated/*']
[checks.z_first]
kind = 'builtin'
profiles = ['fast']
severity = 'critical'
always = true
"#;
struct Fixture {
    root: PathBuf,
    repo: git2::Repository,
}
impl Fixture {
    fn new(native: bool) -> Self {
        let root =
            std::env::temp_dir().join(crate::native_id::new("engine-identity-test").unwrap());
        fs::create_dir(&root).unwrap();
        let repo = git2::Repository::init(root.join("repo")).unwrap();
        let fixture = Self { root, repo };
        fixture.write("candidate.toml", POLICY.as_bytes());
        if native {
            for path in source_fingerprint::EXTRA_FILES {
                fixture.write(path, format!("content {path}\n").as_bytes());
            }
            for name in source_fingerprint::SOURCE_TREES {
                let base = format!("native/{name}");
                for suffix in ["Cargo.toml", "Cargo.lock"] {
                    let path = format!("{base}/{suffix}");
                    fixture.write(&path, format!("content {path}\n").as_bytes());
                }
                fixture.write(
                    &format!("{base}/src/lib.rs"),
                    format!("// {base}\n").as_bytes(),
                );
            }
            fixture.write("native/forge/build.rs", b"fn main() {}\n");
        }
        fixture
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let destination = self.root.join("repo").join(path);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(destination, bytes).unwrap();
        let mut index = self.repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
    }
    fn tree(&self) -> String {
        self.repo.index().unwrap().write_tree().unwrap().to_string()
    }
    fn source(&self) -> Source {
        Source::open(&self.root.join("repo")).unwrap()
    }
    fn snapshot(&self, source: &Source, tree: &str) -> Snapshot {
        Snapshot::create(source, tree, &self.root.join("output"), Limits::default()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn candidate(tree: &str) -> Candidate {
    Candidate {
        kind: Kind::Index,
        tree_oid: tree.into(),
        base_tree_oid: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
        base_commit_oid: None,
        commit_oid: None,
        target_ref: None,
        changes: vec![],
        integration_base_oid: None,
        integration_base_detail: String::new(),
    }
}
fn stamp(value: String) -> BuildStamp {
    BuildStamp {
        source_sha256: Box::leak(value.into_boxed_str()),
        git_commit: "unknown",
        source_matches_git: "0",
    }
}
fn tree_fingerprint(source: &Source, tree: &str) -> Result<(Option<String>, usize)> {
    source_tree_fingerprint(source, &source.list_tree(tree)?)
}
fn policy(snapshot: &Snapshot) -> Policy {
    candidate_policy::load(&snapshot.path(), Some("candidate.toml"), "2026-11-01").unwrap()
}
#[test]
fn git_tree_fingerprint_matches_existing_build_framing_and_ignores_dirty_checkout() {
    let fixture = Fixture::new(true);
    let root = fixture.root.join("repo");
    let expected = crate::source_fingerprint::fingerprint(
        &root,
        &crate::source_fingerprint::files(&root).unwrap(),
    )
    .unwrap();
    let tree = fixture.tree();
    let source = fixture.source();
    let snapshot = fixture.snapshot(&source, &tree);
    let candidate = candidate(&tree);
    let policy = policy(&snapshot);
    fs::write(root.join("native/forge/src/lib.rs"), b"dirty live checkout").unwrap();
    let (evidence, result) = collect(
        &source,
        &snapshot,
        &candidate,
        &policy,
        "2026-11-01",
        &stamp(expected.clone()),
    )
    .unwrap();
    assert_eq!(result.status, CheckStatus::Passed);
    assert_eq!(
        evidence.candidate_source_sha256.as_deref(),
        Some(expected.as_str())
    );
    let meta = fs::metadata("/proc/self/exe").unwrap();
    assert_eq!(
        (
            evidence.executable_dev,
            evidence.executable_inode,
            evidence.executable_bytes
        ),
        (meta.dev(), meta.ino(), meta.len())
    );
    assert_eq!(evidence.executable_sha256.len(), 64);
    assert_eq!(evidence.provenance, "self_observation_not_attestation");
}

#[test]
fn embedded_asset_changes_are_bound_in_git_and_build_fingerprints() {
    let fixture = Fixture::new(true);
    let root = fixture.root.join("repo");
    let source = fixture.source();
    let original = tree_fingerprint(&source, &fixture.tree())
        .unwrap()
        .0
        .unwrap();
    for path in [
        "src/conductor/memory_sources.toml",
        "src/conductor/embedding_routes.toml",
    ] {
        fixture.write(path, b"changed asset\n");
        let changed = tree_fingerprint(&source, &fixture.tree())
            .unwrap()
            .0
            .unwrap();
        let inventory = source_fingerprint::files(&root).unwrap();
        assert_eq!(
            changed,
            source_fingerprint::fingerprint(&root, &inventory).unwrap()
        );
        assert_ne!(changed, original, "{path}");
        fixture.write(path, format!("content {path}\n").as_bytes());
    }
    assert_eq!(
        tree_fingerprint(&source, &fixture.tree())
            .unwrap()
            .0
            .unwrap(),
        original
    );
}
#[test]
fn mismatch_missing_stamp_policy_drift_and_tree_drift_refuse_clean_status() {
    let fixture = Fixture::new(true);
    let source = fixture.source();
    let tree = fixture.tree();
    let snapshot = fixture.snapshot(&source, &tree);
    let mut candidate = candidate(&tree);
    let mut policy = policy(&snapshot);
    let (_, result) = collect(
        &source,
        &snapshot,
        &candidate,
        &policy,
        "2026-11-01",
        &stamp("0".repeat(64)),
    )
    .unwrap();
    assert_eq!(result.status, CheckStatus::Failed);
    assert_eq!(result.findings[0].rule_id, "dirty-engine-source");
    assert!(collect(
        &source,
        &snapshot,
        &candidate,
        &policy,
        "2026-11-01",
        &stamp("unknown".into())
    )
    .is_err());
    candidate.tree_oid = "1".repeat(40);
    assert!(collect(
        &source,
        &snapshot,
        &candidate,
        &policy,
        "2026-11-01",
        &stamp("0".repeat(64))
    )
    .is_err());
    candidate.tree_oid = tree;
    policy.digest = "2".repeat(64);
    assert!(collect(
        &source,
        &snapshot,
        &candidate,
        &policy,
        "2026-11-01",
        &stamp("0".repeat(64))
    )
    .is_err());
}
#[test]
fn consumer_tree_without_native_engine_is_explicitly_unbound() {
    let fixture = Fixture::new(false);
    let source = fixture.source();
    let tree = fixture.tree();
    let snapshot = fixture.snapshot(&source, &tree);
    let (_, result) = collect(
        &source,
        &snapshot,
        &candidate(&tree),
        &policy(&snapshot),
        "2026-11-01",
        &stamp("0".repeat(64)),
    )
    .unwrap();
    assert_eq!(result.status, CheckStatus::Failed);
    assert_eq!(result.findings[0].rule_id, "engine-native-build-unverified");
}
#[test]
fn incomplete_candidate_native_tree_and_captured_policy_mutation_fail_loudly() {
    let fixture = Fixture::new(false);
    fixture.write("native/forge/src/lib.rs", b"missing rest of native inputs");
    let tree = fixture.tree();
    let source = fixture.source();
    assert!(tree_fingerprint(&source, &tree).is_err());
    let snapshot = fixture.snapshot(&source, &tree);
    let policy = policy(&snapshot);
    fs::write(
        snapshot.path().join("candidate.toml"),
        POLICY.replace("max_workers = 2", "max_workers = 1"),
    )
    .unwrap();
    assert!(collect(
        &source,
        &snapshot,
        &candidate(&tree),
        &policy,
        "2026-11-01",
        &stamp("0".repeat(64))
    )
    .is_err());
}
#[test]
fn executable_reader_refuses_missing_oversize_and_in_place_drift() {
    let fixture = Fixture::new(false);
    assert!(File::open(fixture.root.join("missing")).is_err());
    let path = fixture.root.join("image");
    fs::write(&path, b"original").unwrap();
    let file = File::open(&path).unwrap();
    assert!(executable_file(file, || {
        fs::write(&path, b"changed length")?;
        Ok(())
    })
    .is_err());
    fs::File::create(&path)
        .unwrap()
        .set_len(MAX_EXECUTABLE + 1)
        .unwrap();
    assert!(executable_file(File::open(&path).unwrap(), || Ok(())).is_err());
}
#[test]
fn source_symlink_is_refused_but_confined_policy_symlink_remains_supported() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new(true);
    let policy_path = fixture.root.join("repo/candidate.toml");
    fixture.write("config/actual.toml", POLICY.as_bytes());
    fs::remove_file(&policy_path).unwrap();
    symlink("config/actual.toml", &policy_path).unwrap();
    let mut index = fixture.repo.index().unwrap();
    index.add_path(Path::new("candidate.toml")).unwrap();
    index.write().unwrap();
    let tree = fixture.tree();
    let source = fixture.source();
    let expected = tree_fingerprint(&source, &tree).unwrap().0.unwrap();
    let snapshot = fixture.snapshot(&source, &tree);
    let (_, result) = collect(
        &source,
        &snapshot,
        &candidate(&tree),
        &policy(&snapshot),
        "2026-11-01",
        &stamp(expected),
    )
    .unwrap();
    assert_eq!(result.status, CheckStatus::Passed);
    snapshot.discard().unwrap();
    let link = fixture.root.join("repo/native/forge/src/link.rs");
    symlink("lib.rs", &link).unwrap();
    let mut index = fixture.repo.index().unwrap();
    index
        .add_path(Path::new("native/forge/src/link.rs"))
        .unwrap();
    index.write().unwrap();
    assert!(tree_fingerprint(&source, &fixture.tree()).is_err());
}

#[path = "../../forge-cli/source_identity.rs"]
mod build_identity;
#[test]
fn installed_clean_revision_requires_matching_captured_uv_pin() {
    let fixture = Fixture::new(false);
    let pin = "1234567890abcdef1234567890abcdef12345678";
    fixture.write("uv.lock", format!("version = 1\n[[package]]\nname = 'conductor-tooling'\nsource = {{ git = 'https://example.invalid/forge?rev=main#{pin}' }}\n").as_bytes());
    let source = fixture.source();
    let tree = fixture.tree();
    let snapshot = fixture.snapshot(&source, &tree);
    let candidate = candidate(&tree);
    let policy = policy(&snapshot);
    let mut build = BuildStamp {
        source_sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        git_commit: pin,
        source_matches_git: "1",
    };
    let (evidence, result) = collect(
        &source,
        &snapshot,
        &candidate,
        &policy,
        "2026-11-01",
        &build,
    )
    .unwrap();
    assert_eq!(result.status, CheckStatus::Passed);
    assert_eq!(evidence.candidate_git_pin.as_deref(), Some(pin));
    assert_eq!(evidence.mode, "installed");
    build.source_matches_git = "0";
    let (_, result) = collect(
        &source,
        &snapshot,
        &candidate,
        &policy,
        "2026-11-01",
        &build,
    )
    .unwrap();
    assert_eq!(result.findings[0].rule_id, "engine-native-build-unverified");
    build.source_matches_git = "1";
    build.git_commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let (_, result) = collect(
        &source,
        &snapshot,
        &candidate,
        &policy,
        "2026-11-01",
        &build,
    )
    .unwrap();
    assert_eq!(result.findings[0].rule_id, "engine-pin-mismatch");
    fs::write(
        snapshot.path().join("uv.lock"),
        b"changed outside captured Git tree",
    )
    .unwrap();
    assert!(collect(
        &source,
        &snapshot,
        &candidate,
        &policy,
        "2026-11-01",
        &build
    )
    .is_err());
}
#[test]
fn duplicate_or_missing_legacy_pin_never_passes() {
    let value: toml::Value = toml::from_str(
        "[[package]]\nname='conductor-tooling'\n[[package]]\nname='conductor-tooling'\n",
    )
    .unwrap();
    assert!(uv_pin(&value).is_err());
    for raw in [
        "[[package]]\nname='conductor-tooling'\n",
        "[[package]]\nname='conductor-tooling'\nsource={git='x#1234'}\n",
    ] {
        let value: toml::Value = toml::from_str(raw).unwrap();
        assert!(uv_pin(&value).unwrap().is_none());
    }
}
#[test]
fn build_provenance_compares_raw_objects_and_detects_untracked_and_assume_unchanged() {
    let fixture = Fixture::new(true);
    let oid = fixture.repo.index().unwrap().write_tree().unwrap();
    let tree = fixture.repo.find_tree(oid).unwrap();
    let sig = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
    fixture
        .repo
        .commit(Some("HEAD"), &sig, &sig, "native", &tree, &[])
        .unwrap();
    let root = fixture.root.join("repo");
    let head = build_identity::revision(&root).unwrap();
    let files = crate::source_fingerprint::files(&root).unwrap();
    let original = crate::source_fingerprint::fingerprint(&root, &files).unwrap();
    assert!(build_identity::matches_head(
        &root, &head, &files, &original
    ));
    let extra = root.join("native/forge/src/ignored_extra.rs");
    fs::write(root.join(".git/info/exclude"), b"ignored_extra.rs\n").unwrap();
    fs::write(&extra, b"untracked ignored input").unwrap();
    let changed = crate::source_fingerprint::files(&root).unwrap();
    let changed_hash = crate::source_fingerprint::fingerprint(&root, &changed).unwrap();
    assert!(!build_identity::matches_head(
        &root,
        &head,
        &changed,
        &changed_hash
    ));
    fs::remove_file(extra).unwrap();
    let status = std::process::Command::new("git")
        .current_dir(&root)
        .args([
            "update-index",
            "--assume-unchanged",
            "native/forge/src/lib.rs",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    fs::write(
        root.join("native/forge/src/lib.rs"),
        b"changed despite index flag",
    )
    .unwrap();
    let dirty_hash = crate::source_fingerprint::fingerprint(&root, &files).unwrap();
    assert!(!build_identity::matches_head(
        &root,
        &head,
        &files,
        &dirty_hash
    ));
}
