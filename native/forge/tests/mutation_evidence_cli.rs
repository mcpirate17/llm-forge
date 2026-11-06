//! Public reporting behavior over authored repositories without subprocess tools.
#![cfg(target_os = "linux")]

#[path = "support/forge_binary.rs"]
mod forge_binary;
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

struct Fixture {
    dir: tempfile::TempDir,
    tools: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let fixture = Self { dir, tools };
        let manifest =
            "campaigns/fix-mutation-attribution-rootdir_mutation_attribution_fest_20260921.json";
        for (path, text) in [
            (
                manifest,
                include_str!(
                    "../../conductor-native/tests/fixtures/mutation_verifier/manifest.json"
                ),
            ),
            (
                "src/conductor/mutation_attribution.py",
                include_str!(
                    "../../conductor-native/tests/fixtures/mutation_verifier/source.py.txt"
                ),
            ),
            (
                "src/conductor/test_mutation_attribution.py",
                include_str!("../../conductor-native/tests/fixtures/mutation_verifier/test.py.txt"),
            ),
        ] {
            fixture.write(path, text);
        }
        fixture.write(
            "forge.toml",
            "schema_version = 1\n[conductor]\nmutation_registry = 'custom/registry.json'\n",
        );
        fixture.write(
            "custom/registry.json",
            &json!({"schema_version":1,
                "enforcement":"changed_tests", "test_patterns":conductor_native::mutation_evidence::canonical_test_patterns(),
                "receipt_directories":["custom/receipts"], "campaigns":[{"manifest":manifest}]})
            .to_string(),
        );
        fixture.write(
            "tests/test_known.py",
            "def test_known():\n    assert 1 == 1\n",
        );
        fixture.commit();
        fixture
    }
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn commit(&self) {
        let repo = git2::Repository::open(self.root()).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<_> = parent.iter().collect();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "fixture",
            &tree,
            &parents,
        )
        .unwrap();
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(forge_binary::path());
        command
            .current_dir(self.root())
            .args(["mutation"])
            .args(args)
            .env("PATH", self.tools.path())
            .env("HOME", self.tools.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("CONDUCTOR_MUTATION_REGISTRY")
            .env_remove("CONDUCTOR_HOST_ROOT")
            .env_remove("GITHUB_STEP_SUMMARY");
        command
    }
    fn run(&self, args: &[&str]) -> (i32, Value) {
        let output = self.command(args).output().unwrap();
        let code = output.status.code().expect("report exited normally");
        let report = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("invalid report {error}: {output:?}"));
        (code, report)
    }
}

#[test]
fn explicit_coverage_changed_and_canary_keep_distinct_debt_exits() {
    let fixture = Fixture::new();
    let (code, explicit) = fixture.run(&["evidence", "tests/test_known.py"]);
    assert_eq!(code, 5);
    assert_eq!(
        explicit["missing_evidence"][0]["path"],
        "tests/test_known.py"
    );
    let (code, coverage) = fixture.run(&["coverage"]);
    assert_eq!(code, 5);
    assert_eq!(coverage["registry"], "custom/registry.json");
    assert_eq!(coverage["total_test_files"], 2);
    let (code, clean) = fixture.run(&["evidence", "--changed"]);
    assert_eq!(code, 0);
    assert_eq!(clean["checked_test_paths"], json!([]));
    fixture.write(
        "tests/test_known.py",
        "def test_known():\n    assert 2 == 2\n",
    );
    fixture.write("tests/test_new.py", "def test_new():\n    assert True\n");
    let (code, changed) = fixture.run(&["evidence", "--changed"]);
    assert_eq!(code, 6);
    assert_eq!(
        changed["checked_test_paths"],
        json!(["tests/test_known.py", "tests/test_new.py"])
    );
    assert_eq!(
        changed["schema_version"],
        "llm.mutation-testing.changed-evidence.v2"
    );
    let (code, canary) = fixture.run(&["canary"]);
    assert_eq!(code, 0);
    assert_eq!(canary["status"], "FAIL");
    assert_eq!(canary["canary"]["status"], "PASS");
}

#[test]
fn explicit_base_selects_committed_changes_and_unrelated_malformed_receipt_fails_canary() {
    let fixture = Fixture::new();
    let repo = git2::Repository::open(fixture.root()).unwrap();
    let base = repo.head().unwrap().target().unwrap().to_string();
    fixture.write(
        "tests/test_added.py",
        "def test_added():\n    assert True\n",
    );
    fixture.commit();
    let (code, changed) = fixture.run(&["evidence", "--changed", "--base", &base]);
    assert_eq!(code, 6);
    assert_eq!(
        changed["checked_test_paths"],
        json!(["tests/test_added.py"])
    );
    fixture.write("custom/receipts/unrelated.json", "{broken");
    let (code, canary) = fixture.run(&["canary"]);
    assert_eq!(code, 5);
    assert_eq!(
        canary["malformed_receipts"],
        json!(["custom/receipts/unrelated.json"])
    );
    assert_eq!(fixture.run(&["evidence", "--changed"]).0, 5);
}

#[test]
fn domain_refusals_and_clap_conflicts_are_not_passing_reports() {
    let fixture = Fixture::new();
    for args in [
        vec!["evidence"],
        vec!["evidence", "--base", "HEAD"],
        vec!["evidence", "--github"],
    ] {
        let output = fixture.command(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty(), "usage error must not print PASS");
    }
    for args in [
        vec!["evidence", "../test_escape.py"],
        vec!["evidence", "--changed", "--base", "no-such-ref"],
        vec!["coverage", "--registry", "../registry.json"],
    ] {
        let (code, report) = fixture.run(&args);
        assert_eq!(code, 4, "{report}");
        assert_eq!(report["status"], "REFUSED");
        assert!(!report["error"].as_str().unwrap().is_empty());
    }
    let output = fixture
        .command(&["evidence", "--changed", "tests/test_known.py"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let output = fixture
        .command(&["coverage", "--native-runner-pin", "pin.json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn github_output_is_opt_in_and_preserves_existing_summary() {
    let fixture = Fixture::new();
    fixture.write("tests/test_new.py", "def test_new():\n    assert True\n");
    let summary = fixture.tools.path().join("summary.md");
    fs::write(&summary, "existing\n").unwrap();
    let output = fixture
        .command(&["evidence", "--changed", "--github"])
        .env("GITHUB_STEP_SUMMARY", &summary)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(6));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("::warning file=tests/test_new.py::"));
    let text = fs::read_to_string(&summary).unwrap();
    assert!(text.starts_with("existing\n"));
    assert!(text.contains("tests/test_new.py"));
    fixture
        .command(&["evidence", "--changed"])
        .env("GITHUB_STEP_SUMMARY", &summary)
        .output()
        .unwrap();
    assert_eq!(fs::read_to_string(summary).unwrap(), text);
}

#[test]
fn discovery_executes_neither_path_git_nor_configured_fsmonitor() {
    let fixture = Fixture::new();
    let marker = fixture.tools.path().join("executed");
    let script = format!(
        "#!/bin/sh\nprintf invoked > '{}'\nexit 0\n",
        marker.display()
    );
    for name in ["git", "fsmonitor", "python", "python3"] {
        let path = fixture.tools.path().join(name);
        fs::write(&path, &script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let repo = git2::Repository::open(fixture.root()).unwrap();
    repo.config()
        .unwrap()
        .set_str(
            "core.fsmonitor",
            fixture.tools.path().join("fsmonitor").to_str().unwrap(),
        )
        .unwrap();
    fixture.write("tests/test_new.py", "def test_new():\n    assert True\n");
    assert_eq!(fixture.run(&["coverage"]).1["total_test_files"], 3);
    assert_eq!(fixture.run(&["canary"]).1["total_test_files"], 3);
    assert_eq!(
        fixture.run(&["evidence", "--changed"]).1["checked_test_paths"],
        json!(["tests/test_new.py"])
    );
    assert!(
        !marker.exists(),
        "read-only reporting executed an external hook/tool"
    );
}
