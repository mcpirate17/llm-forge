#![cfg(target_os = "linux")]
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::{value, Case};
use forge::{
    candidate_snapshot::{Limits, Snapshot},
    candidate_source::Source,
};
use git2::{Index, Oid, Repository, RepositoryInitOptions, Signature, Time};
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
struct Private {
    // Snapshot must clean up before Case's owned parent is removed.
    snapshot: Snapshot,
    case: Case,
    git: PathBuf,
    index: PathBuf,
    base: String,
    tree: String,
}
fn stage(case: &Case, path: &str, bytes: &[u8]) {
    let full = case.fixture.root.join("repo").join(path);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, bytes).unwrap();
    case.fixture.add(path);
}
fn commit(case: &Case) -> String {
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
    fn new(base: &[(&str, &[u8])], target: &[(&str, &[u8])]) -> Self {
        let case = Case::new(Path::new("/bin/true"));
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
    fn command(&self, files: &[&str]) -> Command {
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
            .args(["candidate", "clang-format", "--repo"])
            .arg(self.snapshot.path())
            .args(["--base", &self.base, "--tree", &self.tree, "--"])
            .args(files);
        command
    }
    fn dry(&self, files: &[&str]) -> Value {
        let output = run(self.command(files));
        assert_code(&output, 0);
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
fn run(mut command: Command) -> Output {
    forge::process_probe::run(&mut command, Duration::from_secs(45), 1024 * 1024).unwrap()
}
fn assert_code(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn private_detached_context_resolves_alternate_base_and_exact_candidate_tree_without_git_path() {
    let f = Private::new(
        &[("a.c", b"int a;\nint b;\n")],
        &[("a.c", b"int a;\nint changed;\n")],
    );
    let before = f.case.source_state();
    fs::write(f.case.fixture.root.join("repo/a.c"), b"dirty original\n").unwrap();
    let report = f.dry(&["a.c"]);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["executed"], false);
    assert_eq!(report["plan"]["candidate_tree_oid"], f.tree);
    assert_eq!(
        report["plan"]["files"][0]["ranges"],
        json!([{"start":2,"end":2}])
    );
    assert_eq!(
        fs::read(f.case.fixture.root.join("repo/a.c")).unwrap(),
        b"dirty original\n"
    );
    let private = Repository::open(&f.git).unwrap();
    let head = private.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.parent_count(), 0);
    assert!(
        private.find_commit(Oid::from_str(&f.base).unwrap()).is_ok(),
        "base must resolve through source ODB alternate"
    );
    f.case.assert_preserved(&before);
}
#[test]
fn private_context_snapshot_drift_and_binary_are_not_silently_formatted_whole_file() {
    let f = Private::new(&[], &[("a.c", b"int a;\n")]);
    let before = f.case.source_state();
    fs::write(f.snapshot.path().join("a.c"), b"drifted snapshot\n").unwrap();
    assert!(!run(f.command(&["a.c"])).status.success());
    f.case.assert_preserved(&before);
    let binary = Private::new(&[], &[("a.c", b"int a;\0\n")]);
    let before = binary.case.source_state();
    let output = run(binary.command(&["a.c"]));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("binary"));
    binary.case.assert_preserved(&before);
}
fn staged_case() -> Case {
    let mut case = Case::new(Path::new(support::forge_binary::path()));
    case.staged_policy(|text|text.replace("timeout_seconds=2","timeout_seconds=30").replace("memory_mb=128","memory_mb=1024")
        .replace("[classes]\n", "[classes]\ncfamily=['*.c']\n")
        .replace("\nclasses=['rust']\ncommand=","\nclasses=['cfamily']\ncommand=")
        .replace("command=['worker','echo','literal argument','{files}']","command=['worker','candidate','clang-format','--base','{base}','--tree','{tree}','--execute','--','{files}']")
        .replace("version_command=['worker','echo','fixture-v1']","version_command=['worker','candidate','clang-format','--tool-version']"));
    case.fixture.stage(
        ".clang-format",
        "BasedOnStyle: LLVM\nIndentWidth: 4\nAllowShortFunctionsOnASingleLine: None\n",
    );
    case.fixture.stage(
        "a.c",
        "int old_drift( ){return 1;}\nint selected(void) {\n    return 2;\n}\n",
    );
    commit(&case);
    case.grants["resources"] = json!({"cpu_millis":2000,"memory_mib":512,"processes":32});
    case.grants["max_wall_seconds"] = json!(40);
    case.grants["max_cpu_seconds"] = json!(40);
    case.grants["max_address_space_mib"] = json!(2048);
    case
}
fn provision(case: &mut Case) -> PathBuf {
    case.delegated();
    let tool = PathBuf::from(
        std::env::var_os("FORGE_TEST_CLANG_FORMAT")
            .expect("explicit native clang-format binary required"),
    )
    .canonicalize()
    .unwrap();
    let mut magic = [0; 4];
    fs::File::open(&tool)
        .unwrap()
        .read_exact(&mut magic)
        .unwrap();
    assert_eq!(&magic, b"\x7fELF");
    case.grants["environment"] = json!({"FORGE_CLANG_FORMAT":tool});
    case.grants["read_only"]
        .as_array_mut()
        .unwrap()
        .push(json!(tool.parent().unwrap()));
    tool
}
fn real_stage(candidate: &str, expected: i32) {
    let mut case = staged_case();
    let _tool = provision(&mut case);
    case.fixture.stage("a.c", candidate);
    let before = case.source_state();
    let dirty = b"dirty original must survive\n";
    fs::write(case.fixture.root.join("repo/a.c"), dirty).unwrap();
    let output = run({
        let mut command = case.command();
        command.arg("--execute");
        command
    });
    let report = value(output, expected);
    assert_eq!(report["files"], json!(["a.c"]));
    assert_eq!(
        report["result"]["status"],
        if expected == 0 { "passed" } else { "failed" }
    );
    assert!(report["result"]["tool_version"]
        .as_str()
        .unwrap()
        .contains("clang-format version"));
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    for stage in stages {
        assert_eq!(stage["report"]["cleanup_complete"], true);
        assert_eq!(stage["report"]["resources"]["verified"], true);
    }
    assert_eq!(fs::read(case.fixture.root.join("repo/a.c")).unwrap(), dirty);
    case.assert_preserved(&before);
}
#[test]
#[ignore = "requires FORGE_TEST_CLANG_FORMAT and explicit delegated FORGE_TEST_CGROUP_PARENT"]
fn real_stage_sealed_index_passes_with_unmodified_old_formatting_drift() {
    real_stage(
        "int old_drift( ){return 1;}\nint selected(void) {\n    return 3;\n}\n",
        0,
    );
}
#[test]
#[ignore = "requires FORGE_TEST_CLANG_FORMAT and explicit delegated FORGE_TEST_CGROUP_PARENT"]
fn real_stage_sealed_index_reports_changed_line_formatting_failure() {
    real_stage(
        "int old_drift( ){return 1;}\nint selected(void) {\nreturn 4;\n}\n",
        1,
    );
}
