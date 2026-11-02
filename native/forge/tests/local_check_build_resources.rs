//! CLI resource-policy regressions using owned Git fixtures and no Cargo children.

#[path = "support/forge_binary.rs"]
mod forge_binary;

use git2::{Oid, Repository, RepositoryInitOptions, Signature};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Fixture {
    owned: TempDir,
    repository: Repository,
}

impl Fixture {
    fn new(policy: &str) -> Self {
        let owned = tempfile::tempdir().unwrap();
        let mut options = RepositoryInitOptions::new();
        options.initial_head("main");
        let repository = Repository::init_opts(owned.path(), &options).unwrap();
        fs::create_dir(owned.path().join(".forge")).unwrap();
        fs::write(owned.path().join(".forge/local-check.toml"), policy).unwrap();
        fs::write(owned.path().join("source.txt"), "base\n").unwrap();
        let fixture = Self { owned, repository };
        let base = fixture.commit(&[".forge/local-check.toml", "source.txt"]);
        fixture
            .repository
            .reference("refs/remotes/origin/main", base, true, "fixture base")
            .unwrap();
        fixture
            .repository
            .branch(
                "feature",
                &fixture.repository.find_commit(base).unwrap(),
                false,
            )
            .unwrap();
        fixture.repository.set_head("refs/heads/feature").unwrap();
        fs::write(fixture.root().join("source.txt"), "feature\n").unwrap();
        fixture.commit(&["source.txt"]);
        fixture
    }

    fn root(&self) -> &Path {
        self.owned.path()
    }

    fn commit(&self, paths: &[&str]) -> Oid {
        let mut index = self.repository.index().unwrap();
        for path in paths {
            index.add_path(Path::new(path)).unwrap();
        }
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = self.repository.find_tree(tree_id).unwrap();
        let signature = Signature::now("fixture", "fixture@example.invalid").unwrap();
        let parent = self
            .repository
            .head()
            .ok()
            .map(|head| head.peel_to_commit().unwrap());
        let parents: Vec<_> = parent.iter().collect();
        self.repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "fixture",
                &tree,
                &parents,
            )
            .unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(forge_binary::path())
            .args(args)
            .current_dir(self.root())
            .env("CARGO_BUILD_JOBS", "64")
            .env("RUST_TEST_THREADS", "64")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap()
    }

    fn run_directory(&self) -> PathBuf {
        let base = self.root().join(".git/forge-checks");
        let latest = fs::read_to_string(base.join("latest")).unwrap();
        base.join(latest.trim())
    }

    fn plan(&self) -> Value {
        serde_json::from_slice(
            &fs::read(self.run_directory().join("build-resources.json")).unwrap(),
        )
        .unwrap()
    }
}

fn assert_pass(output: Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn policy(maximum: Option<usize>, cpus: usize, two_checks: bool) -> String {
    let env = maximum.map_or_else(
        || "[env]\n".to_owned(),
        |maximum| format!("[env]\nCARGO_BUILD_JOBS = \"{maximum}\"\n"),
    );
    let mut text = format!(
        "base_ref = \"origin/main\"\n{env}RUST_TEST_THREADS = \"2\"\n\
         [[setup]]\nname = \"prepare\"\nrun = \"touch .git/setup-ran\"\ntimeout_s = 5\n"
    );
    for name in if two_checks {
        vec!["first", "second"]
    } else {
        vec!["first"]
    } {
        let command = format!(
            "test \"$CARGO_BUILD_JOBS\" = 2 && test \"$RUST_TEST_THREADS\" = 2 && \
             mkdir .git/shared-lock && trap 'rmdir .git/shared-lock' EXIT && \
             sleep 0.1 && printf '%s' \"$CARGO_BUILD_JOBS:$RUST_TEST_THREADS\" > .git/observed-{name}"
        );
        text.push_str(&format!(
            "[[check]]\nname = {name:?}\nrun = {command:?}\ntimeout_s = 5\n\
             [schedule.{name}]\ncpus = {cpus}\nresources = [\"fixture-shared\"]\n"
        ));
    }
    text
}

#[test]
fn base_policy_workers_override_changed_head_and_inherited_environment() {
    let fixture = Fixture::new(&policy(Some(2), 2, false));
    fs::write(
        fixture.root().join(".forge/local-check.toml"),
        policy(Some(8), 2, false),
    )
    .unwrap();
    fixture.commit(&[".forge/local-check.toml"]);
    assert_pass(fixture.run(&["check", "--all"]));
    let plan = fixture.plan();
    assert_eq!(plan["maximum"], 2);
    assert_eq!(plan["workers"], 2);
    assert_eq!(plan["cpu_budget"], 2);
    assert_eq!(plan["memory_mib"], 8192);
    assert!(plan["snapshot"].is_null());
    assert_eq!(
        fs::read_to_string(fixture.root().join(".git/observed-first")).unwrap(),
        "2:2"
    );
    assert_pass(fixture.run(&["verify", "--require-all"]));
    // This supplementary observation does not acquire receipt authority.
    fs::write(
        fixture.run_directory().join("build-resources.json"),
        "untrusted annotation",
    )
    .unwrap();
    assert_pass(fixture.run(&["verify", "--require-all"]));
}

#[test]
fn missing_worker_policy_retains_legacy_default_without_host_snapshot() {
    let fixture = Fixture::new(&policy(None, 2, false));
    assert_pass(fixture.run(&["check", "--all"]));
    let plan = fixture.plan();
    assert_eq!(plan["maximum"], 2);
    assert_eq!(plan["workers"], 2);
    assert_eq!(plan["cpu_budget"], 2);
    assert!(plan["snapshot"].is_null());
    assert_pass(fixture.run(&["verify", "--require-all"]));
}

#[test]
#[ignore = "requires Linux admission with at least 2 effective CPUs and 8192 MiB available RAM"]
fn adaptive_plan_respects_explicit_check_cpu_and_named_resource_limits() {
    let fixture = Fixture::new(&policy(Some(8), 2, true));
    assert_pass(fixture.run(&["check", "--all", "--jobs", "2"]));
    let plan = fixture.plan();
    assert_eq!(plan["maximum"], 8);
    let workers = plan["workers"].as_u64().unwrap();
    assert!((1..=8).contains(&workers));
    assert_eq!(plan["cpu_budget"].as_u64().unwrap(), workers.max(2));
    assert_eq!(plan["memory_mib"].as_u64().unwrap(), workers.max(2) * 4096);
    assert!(plan["snapshot"].is_object());
    if plan["snapshot"]["effective_cpus"].as_f64().unwrap() >= 8.0
        && plan["snapshot"]["memory_available_mib"].as_u64().unwrap() >= 32768
    {
        assert_eq!(
            workers, 8,
            "capable admitted host must reach the policy maximum"
        );
    }
    for name in ["first", "second"] {
        assert_eq!(
            fs::read_to_string(fixture.root().join(format!(".git/observed-{name}"))).unwrap(),
            "2:2"
        );
    }
    assert!(!fixture.root().join(".git/shared-lock").exists());
    assert_pass(fixture.run(&["verify", "--require-all"]));
}

#[test]
fn explicit_cpu_above_policy_maximum_is_rejected_before_setup_or_checks() {
    let fixture = Fixture::new(&policy(Some(8), 9, false));
    let output = fixture.run(&["check", "--all"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cpus"));
    assert!(!fixture.root().join(".git/setup-ran").exists());
    assert!(!fixture.root().join(".git/observed-first").exists());
    assert!(!fixture.root().join(".git/shared-lock").exists());
}

#[test]
fn two_one_cpu_checks_observe_one_build_and_test_worker_and_overlap() {
    let mut text = String::from(
        "base_ref='origin/main'\n[env]\nCARGO_BUILD_JOBS='2'\nRUST_TEST_THREADS='2'\n",
    );
    for (name, peer) in [("first", "second"), ("second", "first")] {
        // A bounded rendezvous requires both checks to run concurrently; a
        // serialized scheduler cannot pass merely by taking longer.
        let command = format!(
            "test \"$CARGO_BUILD_JOBS:$RUST_TEST_THREADS\" = 1:1 && \
             printf '%s' \"$CARGO_BUILD_JOBS:$RUST_TEST_THREADS\" > .git/observed-{name} && \
             touch .git/entered-{name} && \
             i=0; while ! test -e .git/entered-{peer}; do \
             i=$((i+1)); test \"$i\" -lt 200 || exit 42; sleep 0.01; done"
        );
        text.push_str(&format!("[[check]]\nname={name:?}\nrun={command:?}\ntimeout_s=5\n[schedule.{name}]\ncpus=1\nresources=[]\n"));
    }
    let fixture = Fixture::new(&text);
    assert_pass(fixture.run(&["check", "--all", "--jobs", "2"]));
    for name in ["first", "second"] {
        assert_eq!(
            fs::read_to_string(fixture.root().join(format!(".git/observed-{name}"))).unwrap(),
            "1:1"
        );
        assert!(fixture.root().join(format!(".git/entered-{name}")).exists());
    }
    assert_eq!(fixture.plan()["cpu_budget"], 2);
    assert_pass(fixture.run(&["verify", "--require-all"]));
}
#[test]
fn invalid_test_thread_policy_refuses_before_any_check_side_effect() {
    for value in ["0", "-1", "65", "unlimited"] {
        let text = policy(Some(2), 2, false).replace(
            "RUST_TEST_THREADS = \"2\"",
            &format!("RUST_TEST_THREADS = {value:?}"),
        );
        let fixture = Fixture::new(&text);
        let output = fixture.run(&["check", "--all"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("RUST_TEST_THREADS"));
        assert!(!fixture.root().join(".git/setup-ran").exists());
        assert!(!fixture.root().join(".git/observed-first").exists());
    }
}
