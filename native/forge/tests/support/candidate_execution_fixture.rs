//! Shared owned Git and operator-grant fixtures for candidate execution CLI tests.
#[path = "candidate_policy_fixture.rs"]
mod candidate_policy_fixture;
use crate::support::forge_binary;
use candidate_policy_fixture::{policy, Fixture};
use git2::{Repository, Signature};
use serde_json::{json, Value};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};
pub struct Case {
    pub fixture: Fixture,
    pub grants: Value,
}
impl Case {
    pub fn new(program: &Path) -> Self {
        let fixture = Fixture::new();
        let repository = Repository::open(fixture.root.join("repo")).unwrap();
        fixture.stage("baseline.txt", "baseline bytes\n");
        let oid = repository.index().unwrap().write_tree().unwrap();
        let tree = repository.find_tree(oid).unwrap();
        let author = Signature::now("fixture", "fixture@example.invalid").unwrap();
        repository
            .commit(Some("HEAD"), &author, &author, "initial", &tree, &[])
            .unwrap();
        fixture.stage("src/security/test_unit.rs", "fn selected() {}\n");
        let policy = policy("zeta")
            .replace(
                "kind='command'",
                "kind='command'\ntimeout_seconds=2\nmemory_mb=128",
            )
            .replace(
                "\ncommand=['/must-never-be-executed']",
                "\ncommand=['worker','echo','literal argument','{files}']",
            )
            .replace(
                "version_command=['/must-never-be-executed']",
                "version_command=['worker','echo','fixture-v1']",
            );
        fixture.stage("policies/main.toml", &policy);
        std::fs::create_dir(fixture.root.join("cgroup")).unwrap();
        let mut read_only = vec![
            fixture.root.join("repo").canonicalize().unwrap(),
            program.parent().unwrap().canonicalize().unwrap(),
        ];
        for directory in ["/usr", "/lib", "/lib64"] {
            if let Ok(path) = Path::new(directory).canonicalize() {
                read_only.push(path);
            }
        }
        read_only.sort();
        read_only.dedup();
        let grants = json!({"version":1,"source_root":fixture.root.join("repo").canonicalize().unwrap(),
            "read_only":read_only,"programs":{"worker":program.canonicalize().unwrap()},"environment":{},
            "scratch_parent":fixture.root.join("scratch"),"cgroup_parent":fixture.root.join("cgroup"),
            "resources":{"cpu_millis":1000,"memory_mib":256,"processes":16},
            "max_wall_seconds":10,"max_cpu_seconds":10,"max_address_space_mib":512,"python":null});
        Self { fixture, grants }
    }
    pub fn command(&self) -> Command {
        self.command_kind("index")
    }
    pub fn command_kind(&self, kind: &str) -> Command {
        self.command_for("execute", kind, &["zeta"])
    }
    pub fn command_for(&self, action: &str, kind: &str, checks: &[&str]) -> Command {
        let bytes = serde_json::to_vec(&self.grants).unwrap();
        std::fs::write(self.fixture.root.join("grants.json"), bytes).unwrap();
        std::fs::set_permissions(
            self.fixture.root.join("grants.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let mut command = Command::new(forge_binary::path());
        command
            .args(["candidate", action, "--repo"])
            .arg(self.fixture.root.join("repo"))
            .args([
                "--kind",
                kind,
                "--target-ref",
                "HEAD",
                "--policy",
                "policies/main.toml",
                "--profile",
                "fast",
                "--today",
                "2030-01-10",
                "--grants",
            ])
            .arg(self.fixture.root.join("grants.json"))
            .env("PATH", "")
            .env("HOME", &self.fixture.root)
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("CONDUCTOR_POLICY")
            .env_remove("CONDUCTOR_CANDIDATE_POLICY")
            .env_remove("CONDUCTOR_INTEGRATION_REF")
            .env_remove("CONDUCTOR_INTEGRATION_BRANCH");
        for check in checks {
            command.args(["--check", check]);
        }
        command
    }
    pub fn staged_policy(&self, transform: impl FnOnce(String) -> String) {
        let path = self.fixture.root.join("repo/policies/main.toml");
        let content = std::fs::read_to_string(path).unwrap();
        self.fixture
            .stage("policies/main.toml", &transform(content));
    }
    pub fn source_state(&self) -> (Vec<u8>, Vec<u8>, String) {
        let root = self.fixture.root.join("repo");
        let repository = Repository::open(&root).unwrap();
        let head = repository.head().unwrap().target().unwrap().to_string();
        (
            std::fs::read(root.join(".git/index")).unwrap(),
            std::fs::read(root.join(".git/HEAD")).unwrap(),
            head,
        )
    }
    pub fn assert_preserved(&self, before: &(Vec<u8>, Vec<u8>, String)) {
        assert_eq!(&self.source_state(), before);
        self.fixture.clean_scratch();
    }
    pub fn delegated(&mut self) {
        self.grants["cgroup_parent"] = json!(std::path::PathBuf::from(
            std::env::var_os("FORGE_TEST_CGROUP_PARENT")
                .expect("ignored execution tests require FORGE_TEST_CGROUP_PARENT")
        ));
    }
}
pub fn value(output: Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
