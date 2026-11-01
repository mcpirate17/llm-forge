//! Owned private Git metadata shared by clang candidate tool CLI fixtures.
use crate::{candidate_execution_fixture::Case, support};
use forge::{
    candidate_snapshot::{Limits, Snapshot},
    candidate_source::Source,
};
use git2::{Index, Oid, Repository, RepositoryInitOptions, Signature, Time};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
pub struct Private {
    // Snapshot must clean up before Case's owned parent is removed.
    pub snapshot: Snapshot,
    pub case: Case,
    pub git: PathBuf,
    index: PathBuf,
    pub base: String,
    pub tree: String,
}
fn stage(case: &Case, path: &str, bytes: &[u8]) {
    let full = case.fixture.root.join("repo").join(path);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, bytes).unwrap();
    case.fixture.add(path);
}
pub fn commit(case: &Case) -> String {
    let repo = Repository::open(case.fixture.root.join("repo")).unwrap();
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = Signature::now("fixture", "fixture@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[&parent])
        .unwrap()
        .to_string()
}
impl Private {
    pub fn new(base: &[(&str, &[u8])], target: &[(&str, &[u8])]) -> Self {
        Self::from_case(Case::new(Path::new("/bin/true")), base, target)
    }
    pub fn from_case(case: Case, base: &[(&str, &[u8])], target: &[(&str, &[u8])]) -> Self {
        for (path, bytes) in base {
            stage(&case, path, bytes);
        }
        let base_commit = commit(&case);
        for (path, bytes) in target {
            stage(&case, path, bytes);
        }
        let repo = Repository::open(case.fixture.root.join("repo")).unwrap();
        let mut index = repo.index().unwrap();
        index.read(true).unwrap();
        for (path, _) in base {
            if !target.iter().any(|(new, _)| new == path) {
                fs::remove_file(case.fixture.root.join("repo").join(path)).unwrap();
                index.remove_path(Path::new(path)).unwrap();
            }
        }
        index.write().unwrap();
        let tree = index.write_tree().unwrap().to_string();
        let source = Source::open(&case.fixture.root.join("repo")).unwrap();
        let snapshot = Snapshot::create(
            &source,
            &tree,
            &case.fixture.root.join("private-candidate"),
            Limits::default(),
        )
        .unwrap();
        let runtime = case.fixture.root.join("private-runtime");
        fs::create_dir(&runtime).unwrap();
        let git = runtime.join("git");
        let mut options = RepositoryInitOptions::new();
        options.bare(true).external_template(false);
        let private = Repository::init_opts(&git, &options).unwrap();
        let objects = source.common_dir().join("objects").canonicalize().unwrap();
        fs::create_dir_all(git.join("objects/info")).unwrap();
        fs::write(
            git.join("objects/info/alternates"),
            format!("{}\n", objects.display()),
        )
        .unwrap();
        private
            .odb()
            .unwrap()
            .add_disk_alternate(objects.to_str().unwrap())
            .unwrap();
        let target = private.find_tree(Oid::from_str(&tree).unwrap()).unwrap();
        let sig = Signature::new("Forge Candidate", "candidate@invalid", &Time::new(0, 0)).unwrap();
        let head = private
            .commit(None, &sig, &sig, "candidate snapshot", &target, &[])
            .unwrap();
        private.set_head_detached(head).unwrap();
        let index = runtime.join("index");
        let mut staged = Index::open(&index).unwrap();
        staged.read_tree(&target).unwrap();
        staged.write().unwrap();
        for path in [&index, &git.join("HEAD")] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut config = private.config().unwrap();
        for path in [&runtime, &git] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        config.set_bool("core.bare", false).unwrap();
        config
            .set_str("core.worktree", snapshot.path().to_str().unwrap())
            .unwrap();
        fs::write(runtime.join("gitconfig"), b"").unwrap();
        Self {
            snapshot,
            case,
            git,
            index,
            base: base_commit,
            tree,
        }
    }
    pub fn command(&self, files: &[&str]) -> Command {
        self.tool_command("clang-format", files)
    }
    pub fn tool_command(&self, action: &str, files: &[&str]) -> Command {
        let mut command = Command::new(support::forge_binary::path());
        command
            .env_clear()
            .env("PATH", "")
            .env("HOME", &self.case.fixture.root)
            .env("FORGE_LEDGER_DISABLE", "1")
            .env("GIT_DIR", &self.git)
            .env("GIT_WORK_TREE", self.snapshot.path())
            .env("GIT_INDEX_FILE", &self.index)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                self.git.parent().unwrap().join("gitconfig"),
            )
            .current_dir(self.snapshot.path())
            .args(["candidate", action, "--repo"])
            .arg(self.snapshot.path())
            .args(["--base", &self.base, "--tree", &self.tree, "--"])
            .args(files);
        command
    }
    pub fn dry(&self, files: &[&str]) -> Value {
        let output = run(self.command(files));
        assert_code(&output, 0);
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
pub fn run(mut command: Command) -> Output {
    forge::process_probe::run(&mut command, Duration::from_secs(45), 1024 * 1024).unwrap()
}
pub fn assert_code(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
