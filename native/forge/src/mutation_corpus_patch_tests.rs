use super::*;
use crate::candidate_source::Source;
use git2::{Repository, Signature};
use sha2::{Digest, Sha256};
use std::fs;

struct Fixture {
    snapshot: Snapshot,
    _temp: tempfile::TempDir,
    git: PathBuf,
    git_hash: String,
    profile: PathBuf,
    profile_hash: String,
}
impl Fixture {
    fn new(content: &str, patch: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let repo = Repository::init(&root).unwrap();
        fs::create_dir(root.join("pkg")).unwrap();
        fs::create_dir(root.join("patches")).unwrap();
        fs::write(root.join("pkg/file.py"), content).unwrap();
        fs::write(root.join("patches/mut.patch"), patch).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = Signature::now("Fixture", "test@example.invalid").unwrap();
        let commit = repo
            .commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
            .unwrap();
        drop(tree);
        let source = Source::open(&root).unwrap();
        let oid = source.commit_tree(&commit.to_string()).unwrap();
        let snapshot = Snapshot::create(
            &source,
            &oid,
            &temp.path().join("snapshot"),
            Default::default(),
        )
        .unwrap();
        let git = std::env::split_paths(
            &std::env::var_os("PATH").expect("PATH required for Git fixture"),
        )
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .expect("native Git required for patch fixture")
        .canonicalize()
        .unwrap();
        let git_hash = format!("{:x}", Sha256::digest(fs::read(&git).unwrap()));
        let (profile, profile_hash) =
            crate::mutation_corpus_test_support::profile(temp.path(), &git_hash);
        Self {
            _temp: temp,
            snapshot,
            git,
            git_hash,
            profile,
            profile_hash,
        }
    }
    fn held_profile(&self, exclusion: &Path) -> crate::mutation_corpus_git_profile::Profile {
        crate::mutation_corpus_git_profile::Profile::open(
            &self.profile,
            &self.profile_hash,
            &self.git_hash,
            exclusion,
        )
        .unwrap()
    }
    fn checker(&self) -> Checker<'_> {
        Checker::open(
            &self.snapshot,
            &self.git,
            &self.git_hash,
            self.held_profile(&self.snapshot.path()),
        )
        .unwrap()
    }
}
fn patch() -> &'static str {
    "diff --git a/pkg/file.py b/pkg/file.py\n--- a/pkg/file.py\n+++ b/pkg/file.py\n@@ -1,3 +1,3 @@\n before\n-old\n+new\n after\n"
}
fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
#[test]
#[ignore = "requires operator-established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn native_git_check_precedes_anchored_fallback() {
    let exact = Fixture::new("before\nold\nafter\n", patch());
    assert_eq!(
        exact
            .checker()
            .check("patches/mut.patch", &hash(patch()))
            .unwrap(),
        PatchObservation::GitApplies
    );
    let relaxed = Fixture::new("different\nold\nchanged\n", patch());
    assert_eq!(
        relaxed
            .checker()
            .check("patches/mut.patch", &hash(patch()))
            .unwrap(),
        PatchObservation::AnchoredApplies
    );
    let gone = Fixture::new("different\nremoved\nchanged\n", patch());
    assert!(matches!(
        gone.checker()
            .check("patches/mut.patch", &hash(patch()))
            .unwrap(),
        PatchObservation::DoesNotApply { .. }
    ));
}
#[test]
#[ignore = "requires operator-established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn missing_hash_drift_and_bad_pins_do_not_become_passes() {
    let fixture = Fixture::new("before\nold\nafter\n", patch());
    let checker = fixture.checker();
    assert_eq!(
        checker
            .check("patches/missing.patch", &"0".repeat(64))
            .unwrap(),
        PatchObservation::Missing
    );
    assert!(matches!(
        checker.check("patches/mut.patch", &"1".repeat(64)).unwrap(),
        PatchObservation::HashDrift { .. }
    ));
    assert!(checker.check("../outside.patch", &"0".repeat(64)).is_err());
    assert!(Checker::open(
        &fixture.snapshot,
        &fixture.git,
        &"1".repeat(64),
        fixture.held_profile(&fixture.snapshot.path())
    )
    .is_err());
}
#[test]
#[ignore = "requires operator-established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn native_git_accepts_creation_before_narrow_anchored_parser() {
    let patch = "diff --git a/pkg/new.py b/pkg/new.py\nnew file mode 100644\n--- /dev/null\n+++ b/pkg/new.py\n@@ -0,0 +1 @@\n+new\n";
    let fixture = Fixture::new("original\n", patch);
    let checker = fixture.checker();
    assert_eq!(
        checker.check("patches/mut.patch", &hash(patch)).unwrap(),
        PatchObservation::GitApplies
    );
    checker.verify().unwrap();
}
#[test]
#[ignore = "requires operator-established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn previously_checked_patch_target_drift_refuses_verification() {
    let fixture = Fixture::new("before\nold\nafter\n", patch());
    let checker = fixture.checker();
    checker.check("patches/mut.patch", &hash(patch())).unwrap();
    fs::write(fixture.snapshot.path().join("pkg/file.py"), "changed\n").unwrap();
    assert!(checker.verify().is_err());
}
#[test]
#[ignore = "requires operator-established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn live_patch_check_binds_worktree_and_git_info_attributes() {
    let fixture = Fixture::new("before\nold\nafter\n", patch());
    let root = fixture._temp.path().join("repo");
    let checker = Checker::open_live(
        &root,
        &fixture.git,
        &fixture.git_hash,
        fixture.held_profile(&root),
    )
    .unwrap();
    assert_eq!(
        checker.check("patches/mut.patch", &hash(patch())).unwrap(),
        PatchObservation::GitApplies
    );
    let local = root.join("pkg/.gitattributes");
    fs::write(&local, "*.py whitespace=nowarn\n").unwrap();
    assert!(checker.verify().is_err());
    fs::remove_file(local).unwrap();
    let info = root.join(".git/info/attributes");
    fs::create_dir_all(info.parent().unwrap()).unwrap();
    fs::write(info, "*.py whitespace=nowarn\n").unwrap();
    assert!(checker.verify().is_err());
}
#[test]
#[ignore = "requires operator-established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn live_patch_check_binds_configured_attributes_and_configuration() {
    let fixture = Fixture::new("before\nold\nafter\n", patch());
    let root = fixture._temp.path().join("repo");
    let custom = fixture._temp.path().join("configured-attributes");
    fs::write(&custom, "*.py text\n").unwrap();
    let repo = Repository::open(&root).unwrap();
    repo.config()
        .unwrap()
        .set_str("core.attributesfile", custom.to_str().unwrap())
        .unwrap();
    let checker = Checker::open_live(
        &root,
        &fixture.git,
        &fixture.git_hash,
        fixture.held_profile(&root),
    )
    .unwrap();
    checker.check("patches/mut.patch", &hash(patch())).unwrap();
    fs::write(&custom, "*.py -text\n").unwrap();
    assert!(checker.verify().is_err());
    fs::write(&custom, "*.py text\n").unwrap();
    let checker = Checker::open_live(
        &root,
        &fixture.git,
        &fixture.git_hash,
        fixture.held_profile(&root),
    )
    .unwrap();
    checker.check("patches/mut.patch", &hash(patch())).unwrap();
    repo.config()
        .unwrap()
        .set_str("apply.whitespace", "nowarn")
        .unwrap();
    assert!(checker.verify().is_err());
}

#[test]
#[ignore = "requires operator-established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn changed_operator_build_profile_invalidates_the_checked_git_observation() {
    let fixture = Fixture::new("before\nold\nafter\n", patch());
    let checker = fixture.checker();
    checker.check("patches/mut.patch", &hash(patch())).unwrap();
    fs::write(&fixture.profile, b"{}\n").unwrap();
    assert!(checker.verify().is_err());
}
