//! Shared private Git fixtures for native candidate planning and assessment tests.
use git2::{Repository, RepositoryInitOptions};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub struct Fixture {
    pub root: PathBuf,
    repository: Repository,
}
impl Fixture {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(forge::resource_id("forge-policy-cli").unwrap());
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("scratch")).unwrap();
        let mut options = RepositoryInitOptions::new();
        options.initial_head("main");
        let repository = Repository::init_opts(root.join("repo"), &options).unwrap();
        Self { root, repository }
    }
    pub fn stage(&self, path: &str, content: &str) {
        let destination = self.root.join("repo").join(path);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&destination, content).unwrap();
        self.add(path);
    }
    pub fn add(&self, path: &str) {
        let mut index = self.repository.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
    }
    pub fn configured(&self) -> String {
        let policy = policy("zeta");
        self.stage(
            "forge.toml",
            "schema_version=1\n[conductor]\ncandidate_policy='policies/main.toml'\n",
        );
        self.stage("policies/main.toml", &policy);
        self.stage("src/security/test_unit.rs", "fn main() {}\n");
        self.stage("baseline.txt", "baseline bytes\n");
        policy
    }
    pub fn command(&self) -> Command {
        self.command_for("plan")
    }
    pub fn command_for(&self, action: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .args([
                "candidate",
                action,
                "--kind",
                "index",
                "--today",
                "2030-01-10",
                "--scratch-parent",
            ])
            .arg(self.root.join("scratch"))
            .current_dir(self.root.join("repo"))
            .env("PATH", "")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("CONDUCTOR_POLICY")
            .env_remove("CONDUCTOR_CANDIDATE_POLICY")
            .env_remove("CONDUCTOR_INTEGRATION_REF")
            .env_remove("CONDUCTOR_INTEGRATION_BRANCH");
        command
    }
    pub fn clean_scratch(&self) {
        assert_eq!(
            std::fs::read_dir(self.root.join("scratch"))
                .unwrap()
                .count(),
            0
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
pub fn policy(first: &str) -> String {
    format!(
        r#"schema_version=1
block_at='high'
max_workers=2
cache_ttl_days=7
claim_max_age_hours=24
max_file_bytes=1048576
max_binary_bytes=67108864
coverage_threshold=80.0
high_risk_coverage_threshold=95.0
baseline_expires=2030-02-28
[classes]
rust=['src/*.rs']
[risk]
high=['src/security/*']
[paths]
protected_deletes=[]
hot=[]
generated=[]
[checks.{first}]
kind='command'
profiles=['fast','full']
classes=['rust']
command=['/must-never-be-executed']
version_command=['/must-never-be-executed']
[checks.alpha]
kind='builtin'
profiles=['fast']
classes=['cuda']
[checks.full_only]
kind='builtin'
profiles=['full']
always=true
[baselines.code]
path='baseline.txt'
classes=['rust']
required_profiles=['fast','full']
"#
    )
}
