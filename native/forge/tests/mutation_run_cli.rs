//! `forge mutation run` and `inspect` through the installed executable.
//!
//! Public CLI refusal and confined synthetic Cargo protocol tests.
//! The synthetic tools are test fixtures, never mutation evidence.

#![cfg(target_os = "linux")]

#[path = "support/mutation_cli_fixture.rs"]
mod confined_cli;
#[path = "support/forge_binary.rs"]
mod forge_binary;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread::sleep;
use std::time::{Duration, Instant};

const MANIFEST: &str = "campaigns/fixture.json";
const RECEIPT: &str = "campaigns/receipts/run.json";
const REFUSED: i32 = 4;

const PYTHON: &str = r#"#!/bin/sh
case "$STUB_BASELINE" in
  red) echo '1 failed'; exit 1 ;;
  no-module) echo 'No module named pytest' >&2; exit 1 ;;
  no-coverage) exit 0 ;;
esac
: > "$COVERAGE_FILE"
"#;

const FEST: &str = r#"#!/bin/sh
case "$STUB_ENGINE" in
  hang)
    sleep 300 &
    echo $! > "$STUB_DIR/descendant"
    touch "$STUB_DIR/started"
    wait
    ;;
  silent) echo boom >&2; exit 3 ;;
  edit-snapshot) echo '# edited' >> src/a.py ;;
  edit-test) echo '# edited' >> tests/test_a.py ;;
  edit-repository) echo '# edited' >> "$STUB_REPO/src/a.py" ;;
  lock) mkdir locked; : > locked/held; chmod 500 locked ;;
esac
sed "s#__WORKTREE__#$PWD#g" "$STUB_DIR/report.json"
exit "${STUB_EXIT:-0}"
"#;

const CARGO_MUTANTS: &str = r#"#!/bin/sh
while [ $# -gt 0 ]; do
  [ "$1" = --output ] && output=$2
  shift
done
mkdir -p "$output/mutants.out"
cp "$STUB_DIR/report.json" "$output/mutants.out/outcomes.json"
exit "${STUB_EXIT:-0}"
"#;

/// Writing an executable while another thread forks leaves the write descriptor
/// open in the child for an instant, and exec then fails with ETXTBSY.
static FORKS: Mutex<()> = Mutex::new(());

fn forks() -> MutexGuard<'static, ()> {
    FORKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "{args:?}: {output:?}");
}

fn digest(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).expect("pinned file")))
}

fn stub(path: &Path, body: &str) {
    fs::write(path, body).expect("stub");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A host repository on a branch that changed `src/a.py`, with stub tools beside it.
struct Host {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    stubs: PathBuf,
}

/// What one invocation left behind.
struct Run {
    code: Option<i32>,
    printed: Value,
    receipt: Value,
}

impl Host {
    fn new() -> Self {
        let _forks = forks();
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        let (repo, stubs) = (root.join("repo"), root.join("stubs"));
        fs::create_dir_all(repo.join("src")).expect("src");
        fs::create_dir_all(repo.join("tests")).expect("tests");
        fs::create_dir_all(repo.join("campaigns")).expect("campaigns");
        fs::create_dir_all(&stubs).expect("stubs");
        stub(&stubs.join("python"), PYTHON);
        stub(&stubs.join("fest"), FEST);
        stub(&stubs.join("cargo-mutants"), CARGO_MUTANTS);
        fs::write(
            repo.join("pyproject.toml"),
            format!(
                "[project]\nname = \"host\"\n\n[tool.conductor]\n\
                 snapshot_python = \"{}/python\"\n\
                 mutation_registry = \"campaigns/registry.json\"\n",
                stubs.display()
            ),
        )
        .expect("pyproject");
        fs::write(repo.join("src/a.py"), "x = 1\n").expect("source");
        fs::write(repo.join("tests/test_a.py"), "def test_a(): pass\n").expect("test");
        fs::write(repo.join(".gitignore"), "campaigns/receipts/\n").expect("ignore");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        git(&repo, &["checkout", "-q", "-b", "feat"]);
        fs::write(repo.join("src/a.py"), "x = 2\n").expect("change");
        git(&repo, &["commit", "-qam", "change"]);
        Self {
            _dir: dir,
            repo,
            stubs,
        }
    }

    /// The fest campaign for this branch's change, top-level keys replaced.
    fn campaign(&self, overrides: Value) {
        let mut manifest = json!({
            "campaign_id": "fixture",
            "title": "fixture",
            "language": "python",
            "mutation_engine": "fest",
            "generator": {"source": ["src/a.py"], "run_timeout_seconds": 60},
            "test_argv": ["python", "-m", "pytest", "tests/test_a.py"],
            "source_sha256": {"src/a.py": digest(&self.repo.join("src/a.py"))},
            "test_sha256": {"tests/test_a.py": digest(&self.repo.join("tests/test_a.py"))},
        });
        for (key, value) in overrides.as_object().expect("object") {
            manifest[key] = value.clone();
        }
        let body = serde_json::to_string_pretty(&manifest).expect("manifest");
        fs::write(self.repo.join(MANIFEST), body).expect("manifest");
        self.commit("campaign");
    }

    /// Commit everything: a dirty checkout with no claim is refused before any
    /// other check, which would hide the refusal a test is looking for.
    fn commit(&self, message: &str) {
        let _forks = forks();
        git(&self.repo, &["add", "."]);
        git(&self.repo, &["commit", "-q", "-m", message]);
    }

    fn command(&self, args: &[&str], environment: &[(&str, &str)]) -> Command {
        let mut command = Command::new(forge_binary::path());
        command
            .args(args)
            .current_dir(&self.repo)
            .env("STUB_DIR", &self.stubs)
            .env("STUB_REPO", &self.repo)
            .env("PATH", format!("{}:{}", self.stubs.display(), host_path()))
            .env_remove("CONDUCTOR_MUTATION_REGISTRY")
            .env_remove("STUB_ENGINE")
            .env_remove("STUB_BASELINE")
            .env_remove("STUB_EXIT")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.envs(environment.iter().copied());
        command
    }

    fn spawn(&self, environment: &[(&str, &str)]) -> Child {
        let mut command = self.command(
            &[
                "mutation",
                "run",
                MANIFEST,
                "--allow-mutations",
                "--owner",
                "lane",
                "--base",
                "main",
                "--receipt",
                RECEIPT,
            ],
            environment,
        );
        let _forks = forks();
        command.spawn().expect("forge starts")
    }

    fn finish(&self, child: Child) -> Run {
        let output = child.wait_with_output().expect("forge finishes");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let printed = serde_json::from_str(&stdout).unwrap_or_else(|error| {
            panic!(
                "stdout is not JSON ({error}): {stdout}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        let receipt = fs::read_to_string(self.repo.join(RECEIPT))
            .map(|text| serde_json::from_str(&text).expect("the receipt is JSON"))
            .unwrap_or(Value::Null);
        Run {
            code: output.status.code(),
            printed,
            receipt,
        }
    }

    fn run(&self, environment: &[(&str, &str)]) -> Run {
        self.finish(self.spawn(environment))
    }

    fn cli(&self, args: &[&str]) -> Run {
        let mut command = self.command(args, &[]);
        let child = {
            let _forks = forks();
            command.spawn().expect("forge starts")
        };
        self.finish(child)
    }
}

fn host_path() -> String {
    std::env::var("PATH").expect("PATH")
}

fn error_of(run: &Run) -> &str {
    run.printed["error"].as_str().unwrap_or_default()
}

#[test]
fn nothing_runs_without_the_explicit_opt_in() {
    let host = Host::new();
    host.campaign(json!({}));
    let run = host.cli(&[
        "mutation", "run", MANIFEST, "--owner", "lane", "--base", "main",
    ]);
    assert_eq!(run.code, Some(REFUSED));
    assert_eq!(
        error_of(&run),
        "refusing mutation run without --allow-mutations"
    );
    assert!(!host.repo.join("campaigns/receipts").exists());
}

#[test]
fn a_campaign_over_unchanged_files_is_refused_before_a_receipt_opens() {
    let host = Host::new();
    host.campaign(json!({}));
    {
        let _forks = forks();
        git(&host.repo, &["update-ref", "refs/heads/main", "HEAD"]);
    }
    let run = host.run(&[]);
    assert_eq!(run.code, Some(REFUSED));
    assert!(
        error_of(&run).contains("outside this agent's changes"),
        "{}",
        error_of(&run)
    );
    assert!(run.receipt.is_null());
}

#[test]
fn an_unclaimed_edit_in_the_checkout_is_refused_before_a_receipt_opens() {
    let host = Host::new();
    host.campaign(json!({}));
    fs::write(host.repo.join("src/a.py"), "x = 3\n").expect("edit");
    let run = host.run(&[]);
    assert_eq!(run.code, Some(REFUSED));
    assert!(
        error_of(&run).contains("no active ownership claim"),
        "{}",
        error_of(&run)
    );
    assert!(run.receipt.is_null());
}

fn cargo_outcome(summary: &str, line: u64) -> Value {
    json!({
        "scenario": {"Mutant": {
            "name": format!("src/a.py:{line}: replace"),
            "package": "fixture",
            "file": "src/a.py",
            "function": {"function_name": "f", "return_type": ""},
            "span": {"start": {"line": line, "column": 1}, "end": {"line": line, "column": 2}},
            "replacement": line.to_string(),
            "genre": "FnValue",
        }},
        "summary": summary,
        "phase_results": [],
    })
}

fn cargo_report(baseline: &str, summaries: &[&str]) -> Value {
    let mut outcomes =
        vec![json!({"scenario": "Baseline", "summary": baseline, "phase_results": []})];
    for (line, summary) in summaries.iter().enumerate() {
        outcomes.push(cargo_outcome(summary, line as u64 + 1));
    }
    json!({
        "total_mutants": summaries.len(),
        "cargo_mutants_version": "stub",
        "outcomes": outcomes,
    })
}

#[test]
fn inspection_refuses_readiness_and_never_launches_payload() {
    let host = Host::new();
    host.campaign(json!({}));
    let initial = host.cli(&["mutation", "inspect", MANIFEST]);
    assert_eq!(initial.code, Some(3));
    assert_eq!(initial.printed["status"], "NOT_READY");
    assert!(initial.printed["readiness_reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row
            .as_str()
            .unwrap()
            .contains("explicit pinned execution grants")));
    fs::write(host.repo.join("src/a.py"), "x = 9\n").unwrap();
    let drift = host.cli(&["mutation", "inspect", MANIFEST]);
    assert_eq!(drift.code, Some(3));
    assert!(drift.printed["readiness_reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row.as_str().unwrap().contains("pinned source drifted")));
    let absent = host.cli(&["mutation", "inspect", "campaigns/absent.json"]);
    assert_eq!(absent.code, Some(REFUSED));
    assert!(!host.repo.join("campaigns/receipts").exists());
}

#[test]
fn unsupported_fest_refuses_before_payload_or_receipt() {
    let host = Host::new();
    host.campaign(json!({}));
    let run = host.run(&[]);
    assert_eq!(run.code, Some(REFUSED));
    assert!(error_of(&run).contains("ordinary mutation profile is not yet supported"));
    assert!(run.receipt.is_null());
    assert!(!host.repo.join("campaigns/receipts").exists());
}
