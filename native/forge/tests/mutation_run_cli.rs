//! `forge mutation run` and `inspect` through the installed executable.
//!
//! Stub `python`, `fest` and `cargo-mutants` scripts force each way a run can go
//! wrong. They prove what the runner refuses; they are never mutation evidence.
//! What the real engines do is measured by running them.

#![cfg(target_os = "linux")]

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

    fn report(&self, report: Value) {
        fs::write(self.stubs.join("report.json"), report.to_string()).expect("report");
    }

    /// One fest report of `statuses`, its totals consistent with them.
    fn results(&self, statuses: &[&str]) {
        self.report(fest_report(statuses, statuses.len(), statuses.len()));
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

    fn manifest_text(&self) -> String {
        fs::read_to_string(self.repo.join(MANIFEST)).expect("manifest")
    }

    fn descendant(&self) -> String {
        fs::read_to_string(self.stubs.join("descendant"))
            .expect("the stub recorded its descendant")
            .trim()
            .to_string()
    }

    fn wait_for_engine(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.stubs.join("started").exists() {
            assert!(Instant::now() < deadline, "the engine never started");
            sleep(Duration::from_millis(10));
        }
    }

    /// What every ending owes: no snapshot, no registered child, source untouched.
    fn assert_cleaned_up(&self, run: &Run) {
        let root = run.receipt["snapshot"]["root"]
            .as_str()
            .expect("a snapshot was made");
        assert!(!Path::new(root).exists(), "{root} outlived the run");
        assert_eq!(run.receipt["snapshot"]["removed"], true);
        let registry = self
            .repo
            .join("campaigns/receipts/.iterations/live_pgids.json");
        if registry.exists() {
            let held: Value =
                serde_json::from_str(&fs::read_to_string(registry).expect("registry"))
                    .expect("registry is JSON");
            assert_eq!(held, json!([]), "a child is still registered");
        }
        assert_eq!(
            fs::read_to_string(self.repo.join("src/a.py")).expect("source"),
            "x = 2\n"
        );
    }
}

fn host_path() -> String {
    std::env::var("PATH").expect("PATH")
}

fn fest_report(statuses: &[&str], generated: usize, tested: usize) -> Value {
    let results: Vec<Value> = statuses
        .iter()
        .enumerate()
        .map(|(index, status)| {
            json!({
                "mutant": {
                    "file_path": "__WORKTREE__/src/a.py",
                    "line": 1,
                    "column": 5,
                    "byte_offset": 4,
                    "byte_length": 1,
                    "original_text": "2",
                    "mutated_text": index.to_string(),
                    "mutator_name": "constant_replace",
                },
                "status": status,
                "tests_run": [],
                "duration": {"secs": 0, "nanos": 1_000_000},
            })
        })
        .collect();
    json!({
        "mutants_generated": generated,
        "mutants_tested": tested,
        "results": results,
    })
}

fn process_ended(pid: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => return true,
            Ok(stat)
                if stat
                    .rsplit(')')
                    .next()
                    .is_some_and(|rest| rest.trim().starts_with('Z')) =>
            {
                return true;
            }
            Ok(_) if Instant::now() > deadline => return false,
            Ok(_) => sleep(Duration::from_millis(10)),
        }
    }
}

fn error_of(run: &Run) -> &str {
    run.printed["error"].as_str().unwrap_or_default()
}

/// A run the fixture drove to a refusal, with the receipt saying so.
fn assert_refused(host: &Host, run: &Run, status: &str, needle: &str) {
    assert_eq!(run.code, Some(REFUSED), "{:?}", run.printed);
    assert_eq!(run.printed["status"], "REFUSED");
    assert!(error_of(run).contains(needle), "{}", error_of(run));
    assert_eq!(run.receipt["status"], status);
    host.assert_cleaned_up(run);
}

fn assert_verdict(host: &Host, run: &Run, code: i32, status: &str) {
    assert_eq!(run.code, Some(code), "{:?}", run.printed);
    assert_eq!(run.printed["status"], status);
    assert_eq!(run.receipt["status"], status);
    host.assert_cleaned_up(run);
}

#[test]
fn a_clean_run_passes_and_is_fully_accounted() {
    let host = Host::new();
    host.campaign(json!({}));
    host.results(&["Killed", "Killed"]);
    let run = host.run(&[]);
    assert_verdict(&host, &run, 0, "PASS");
    assert_eq!(
        run.receipt["accounting"],
        json!({"planned": 2, "completed": 2, "failed": 0, "censored": 0, "unstarted": 0})
    );
    assert_eq!(run.receipt["complete_campaign"], true);
    assert_eq!(
        run.receipt["identity_check"],
        json!({"snapshot_drift": {}, "repository_drift": {}})
    );
    assert_eq!(run.receipt["runner_kind"], "forge-native");
    assert_eq!(run.receipt["receipt_path"], RECEIPT);
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
fn a_receipt_outside_the_repository_is_refused() {
    let host = Host::new();
    host.campaign(json!({}));
    let run = host.cli(&[
        "mutation",
        "run",
        MANIFEST,
        "--allow-mutations",
        "--owner",
        "lane",
        "--base",
        "main",
        "--receipt",
        "/tmp/elsewhere.json",
    ]);
    assert_eq!(run.code, Some(REFUSED));
    assert_eq!(error_of(&run), "receipt path must be inside the repository");
}

#[test]
fn a_red_baseline_stops_the_run_and_records_nothing_in_the_manifest() {
    let host = Host::new();
    host.campaign(json!({}));
    let before = host.manifest_text();
    let run = host.run(&[("STUB_BASELINE", "red")]);
    assert_refused(&host, &run, "BASELINE_FAILED", "unmutated baseline failed");
    assert_eq!(host.manifest_text(), before);
    assert_eq!(run.receipt["baseline"]["returncode"], 1);
}

#[test]
fn a_baseline_that_could_not_start_python_says_how_to_fix_it() {
    let host = Host::new();
    host.campaign(json!({}));
    let run = host.run(&[("STUB_BASELINE", "no-module")]);
    assert_refused(&host, &run, "BASELINE_FAILED", "CONDUCTOR_SNAPSHOT_PYTHON");
}

#[test]
fn a_baseline_with_no_coverage_database_is_refused() {
    let host = Host::new();
    host.campaign(json!({}));
    let run = host.run(&[("STUB_BASELINE", "no-coverage")]);
    assert_refused(
        &host,
        &run,
        "ERROR",
        "baseline produced no coverage database",
    );
}

#[test]
fn an_engine_that_outlives_its_bound_is_killed_with_everything_it_started() {
    let host = Host::new();
    host.campaign(json!({"generator": {"source": ["src/a.py"], "run_timeout_seconds": 1}}));
    let run = host.run(&[("STUB_ENGINE", "hang")]);
    assert_refused(&host, &run, "ERROR", "fest timed out after 1s");
    assert_eq!(run.receipt["engine_result"]["timed_out"], true);
    assert!(
        process_ended(&host.descendant()),
        "the engine's child outlived it"
    );
}

#[test]
fn a_terminated_run_is_cancelled_and_leaves_nothing_running() {
    let host = Host::new();
    host.campaign(json!({}));
    let child = host.spawn(&[("STUB_ENGINE", "hang")]);
    host.wait_for_engine();
    let forge = i32::try_from(child.id()).expect("pid");
    // SAFETY: `forge` is this test's own unreaped child, so the pid is still its.
    assert_eq!(unsafe { libc::kill(forge, libc::SIGTERM) }, 0);
    let run = host.finish(child);
    assert_verdict(&host, &run, 1, "CANCELLED");
    assert_eq!(run.receipt["cancelled_by_signal"], libc::SIGTERM);
    assert!(
        run.receipt["accounting"]["unstarted"].is_null(),
        "unknown is not zero"
    );
    assert!(
        process_ended(&host.descendant()),
        "the engine's child outlived it"
    );
}

#[test]
fn an_engine_that_printed_no_report_is_refused() {
    let host = Host::new();
    host.campaign(json!({}));
    let run = host.run(&[("STUB_ENGINE", "silent")]);
    assert_refused(
        &host,
        &run,
        "ERROR",
        "fest produced no JSON report (rc=3): boom",
    );
}

#[test]
fn a_host_that_drifted_before_the_run_is_refused() {
    let host = Host::new();
    host.campaign(json!({}));
    fs::write(host.repo.join("src/a.py"), "x = 3\n").expect("drift");
    host.commit("drift");
    let run = host.run(&[]);
    assert_eq!(run.code, Some(REFUSED));
    assert!(error_of(&run).contains("drifted"), "{}", error_of(&run));
    assert_eq!(run.receipt["status"], "ERROR");
    assert!(!Path::new(run.receipt["snapshot"]["root"].as_str().expect("root")).exists());
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

fn drift_during(engine: &str, key: &str, target: &str, message: &str) {
    let host = Host::new();
    host.campaign(json!({}));
    host.results(&["Killed"]);
    let child = host.spawn(&[("STUB_ENGINE", engine)]);
    let run = host.finish(child);
    assert_eq!(run.code, Some(REFUSED), "{:?}", run.printed);
    assert!(error_of(&run).contains(message), "{}", error_of(&run));
    assert_eq!(run.receipt["status"], "ERROR");
    assert!(run.receipt["identity_check"][key][target].is_string());
    assert!(!Path::new(run.receipt["snapshot"]["root"].as_str().expect("root")).exists());
}

#[test]
fn a_source_edited_inside_the_snapshot_during_the_run_is_refused() {
    drift_during(
        "edit-snapshot",
        "snapshot_drift",
        "src/a.py",
        "pinned bytes changed inside the snapshot",
    );
}

#[test]
fn a_test_edited_inside_the_snapshot_during_the_run_is_refused() {
    drift_during(
        "edit-test",
        "snapshot_drift",
        "tests/test_a.py",
        "pinned bytes changed inside the snapshot",
    );
}

#[test]
fn a_repository_edited_during_the_run_is_refused() {
    drift_during(
        "edit-repository",
        "repository_drift",
        "src/a.py",
        "pinned bytes changed in the repository",
    );
}

#[test]
fn survivors_hold_the_ratchet_once_then_a_new_one_fails() {
    let host = Host::new();
    host.campaign(json!({}));
    host.results(&["Killed", "Survived"]);
    let first = host.run(&[]);
    assert_verdict(&host, &first, 0, "RATCHET_HELD");
    let recorded: Value = serde_json::from_str(&host.manifest_text()).expect("manifest");
    let survivors = first.printed["survivors"].clone();
    assert_eq!(survivors.as_array().expect("list").len(), 1);
    assert_eq!(recorded["survivor_baseline"], survivors);
    host.commit("record the survivor baseline");

    let held = host.run(&[]);
    assert_verdict(&host, &held, 0, "RATCHET_HELD");
    assert_eq!(held.printed["new_survivors"], json!([]));

    host.results(&["Killed", "Survived", "Survived"]);
    let worse = host.run(&[]);
    assert_verdict(&host, &worse, 1, "FAIL");
    assert_eq!(
        worse.printed["new_survivors"]
            .as_array()
            .expect("list")
            .len(),
        1
    );
}

#[test]
fn a_censored_row_is_incomplete_whatever_else_was_killed() {
    for status in ["Timeout", "NoCoverage"] {
        let host = Host::new();
        host.campaign(json!({}));
        host.results(&["Killed", status]);
        let run = host.run(&[]);
        assert_verdict(&host, &run, 1, "INCOMPLETE");
        assert_eq!(run.receipt["accounting"]["censored"], 1);
    }
}

#[test]
fn an_error_row_is_an_error() {
    let host = Host::new();
    host.campaign(json!({}));
    host.results(&["Killed", "Error"]);
    let run = host.run(&[]);
    assert_verdict(&host, &run, 1, "ERROR");
    assert_eq!(run.receipt["accounting"]["failed"], 1);
    assert_eq!(run.receipt["complete_campaign"], false);
}

#[test]
fn an_unknown_status_is_refused() {
    let host = Host::new();
    host.campaign(json!({}));
    host.results(&["Killed", "Pending"]);
    let run = host.run(&[]);
    assert_refused(
        &host,
        &run,
        "ERROR",
        "fest reported unknown status 'Pending'",
    );
}

#[test]
fn a_mutant_the_engine_planned_but_never_reported_is_unstarted() {
    let host = Host::new();
    host.campaign(json!({}));
    host.report(fest_report(&["Killed", "Killed"], 3, 2));
    let run = host.run(&[]);
    assert_verdict(&host, &run, 1, "INCOMPLETE");
    assert_eq!(run.receipt["accounting"]["unstarted"], 1);
    assert_eq!(run.receipt["complete_campaign"], false);
}

#[test]
fn a_survivor_from_an_unfinished_campaign_is_not_recorded_as_the_baseline() {
    let host = Host::new();
    host.campaign(json!({}));
    let before = host.manifest_text();
    host.report(fest_report(&["Killed", "Survived"], 3, 2));
    let run = host.run(&[]);
    assert_verdict(&host, &run, 1, "FAIL");
    assert_eq!(run.receipt["accounting"]["unstarted"], 1);
    assert_eq!(host.manifest_text(), before);
}

#[test]
fn more_rows_than_the_plan_are_refused() {
    let host = Host::new();
    host.campaign(json!({}));
    host.report(fest_report(&["Killed", "Killed"], 1, 2));
    let run = host.run(&[]);
    assert_refused(
        &host,
        &run,
        "ERROR",
        "reported 2 mutants against a plan of 1",
    );
}

#[test]
fn a_run_that_generated_or_tested_nothing_is_refused() {
    let host = Host::new();
    host.campaign(json!({}));
    host.report(fest_report(&[], 0, 0));
    let run = host.run(&[]);
    assert_refused(&host, &run, "ERROR", "the engine generated no mutants");
    host.report(fest_report(&["NoCoverage"], 1, 0));
    let run = host.run(&[]);
    assert_refused(&host, &run, "ERROR", "no mutant was executed");
}

#[test]
fn a_fest_failure_is_refused_whatever_its_report_says() {
    for statuses in [&["Killed", "Killed"], &["Killed", "Survived"]] {
        let host = Host::new();
        host.campaign(json!({}));
        let before = host.manifest_text();
        host.results(statuses);
        let run = host.run(&[("STUB_EXIT", "1")]);
        assert_refused(&host, &run, "ERROR", "fest failed (rc=1)");
        assert_eq!(run.receipt["engine_result"]["returncode"], 1);
        assert_eq!(host.manifest_text(), before);
    }
}

#[test]
fn a_snapshot_that_survives_cleanup_is_not_a_pass() {
    let host = Host::new();
    host.campaign(json!({}));
    host.results(&["Killed"]);
    let before = host.manifest_text();
    let run = host.run(&[("STUB_ENGINE", "lock")]);
    let root = PathBuf::from(run.receipt["snapshot"]["root"].as_str().expect("root"));
    let locked = root.join("worktree/locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).expect("unlock");
    fs::remove_dir_all(&root).expect("clean the survivor");
    assert_eq!(run.code, Some(REFUSED), "{:?}", run.printed);
    assert_eq!(run.receipt["status"], "ERROR");
    assert_eq!(run.receipt["snapshot"]["removed"], false);
    assert!(run.receipt["snapshot"]["removal_error"]
        .as_str()
        .expect("the reason")
        .contains("survived removal"));
    assert_eq!(
        host.manifest_text(),
        before,
        "no baseline from an unverified run"
    );
}

#[test]
fn inspection_reports_readiness_without_running_anything() {
    let host = Host::new();
    host.campaign(json!({}));
    let ready = host.cli(&["mutation", "inspect", MANIFEST]);
    assert_eq!(ready.code, Some(0), "{:?}", ready.printed);
    assert_eq!(ready.printed["status"], "READY");
    fs::write(host.repo.join("src/a.py"), "x = 3\n").expect("drift");
    let drifted = host.cli(&["mutation", "inspect", MANIFEST]);
    assert_eq!(drifted.code, Some(3));
    assert_eq!(
        drifted.printed["readiness_reasons"],
        json!(["pinned source drifted: src/a.py"])
    );
    let absent = host.cli(&["mutation", "inspect", "campaigns/absent.json"]);
    assert_eq!(absent.code, Some(REFUSED));
    assert!(
        !host.repo.join("campaigns/receipts").exists(),
        "inspect runs nothing"
    );
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

fn cargo_host(report: Value) -> Host {
    let host = Host::new();
    host.campaign(json!({
        "language": "rust",
        "mutation_engine": "cargo-mutants",
        "generator": {
            "source": ["src/a.py"],
            "run_timeout_seconds": 60,
            "options": {"manifest_path": "Cargo.toml"},
        },
        "test_argv": ["/bin/true"],
    }));
    host.report(report);
    host
}

#[test]
fn cargo_mutants_reporting_a_survivor_is_a_verdict_not_a_failure() {
    let host = cargo_host(cargo_report("Success", &["CaughtMutant", "MissedMutant"]));
    let run = host.run(&[("STUB_EXIT", "2")]);
    assert_verdict(&host, &run, 0, "RATCHET_HELD");
}

#[test]
fn cargo_mutants_reporting_a_timeout_is_incomplete() {
    let host = cargo_host(cargo_report("Success", &["CaughtMutant", "Timeout"]));
    let run = host.run(&[("STUB_EXIT", "3")]);
    assert_verdict(&host, &run, 1, "INCOMPLETE");
}

#[test]
fn a_cargo_mutants_exit_that_is_no_verdict_is_refused_despite_its_report() {
    for code in ["1", "4", "70"] {
        let host = cargo_host(cargo_report("Success", &["CaughtMutant", "MissedMutant"]));
        let run = host.run(&[("STUB_EXIT", code)]);
        assert_refused(
            &host,
            &run,
            "ERROR",
            &format!("cargo-mutants failed (rc={code})"),
        );
    }
}

#[test]
fn a_cargo_mutants_exit_its_report_cannot_explain_is_incomplete() {
    for code in ["2", "3"] {
        let host = cargo_host(cargo_report("Success", &["CaughtMutant", "CaughtMutant"]));
        let run = host.run(&[("STUB_EXIT", code)]);
        assert_verdict(&host, &run, 1, "INCOMPLETE");
    }
}

#[test]
fn a_red_cargo_mutants_baseline_is_refused() {
    let host = cargo_host(cargo_report("Failure", &["CaughtMutant"]));
    let run = host.run(&[("STUB_EXIT", "4")]);
    assert_refused(&host, &run, "ERROR", "unmutated baseline failed: Failure");
}
