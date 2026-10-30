#![cfg(feature = "source-analysis")]

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "dead-code-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dead-code-audit"));
        command.arg("--root").arg(&self.0);
        command
    }

    fn replay(&self, args: &[&str]) -> Output {
        self.command()
            .args(["--vulture-output", "findings.txt"])
            .args(args)
            .output()
            .unwrap()
    }

    fn report(&self) -> Value {
        serde_json::from_slice(
            &fs::read(self.0.join(".forge/reports/dead-code/latest.json")).unwrap(),
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn replay_reports_all_verdicts_and_does_not_read_its_previous_report() {
    let fixture = Fixture::new();
    fixture.write("source.py", "alive = test_only = doc_only = dead = 1\n");
    fixture.write("prod.py", "from source import alive as renamed\n");
    fixture.write("tests/use.py", "source.test_only()\n");
    fixture.write("README.md", "Use source.doc_only.\n");
    let findings = ["alive", "test_only", "doc_only", "dead"]
        .map(|name| format!("source.py:1: unused variable '{name}' (60% confidence)\n"))
        .join("");
    fixture.write("findings.txt", &findings);
    assert_success(&fixture.replay(&[]));
    let first = fixture.report();
    assert_eq!(first["complete"], true);
    assert_eq!(first["counts"]["ALIVE"], 1);
    assert_eq!(first["counts"]["TEST-ONLY"], 1);
    assert_eq!(first["counts"]["DOC-ONLY"], 1);
    assert_eq!(first["counts"]["DEAD"], 1);
    assert_eq!(first["invocation"]["vulture_exit_code"], Value::Null);
    assert_success(&fixture.replay(&[]));
    assert_eq!(first["findings"], fixture.report()["findings"]);
}

#[test]
fn invalid_python_produces_an_incomplete_report_and_nonzero_status() {
    let fixture = Fixture::new();
    fixture.write(
        "findings.txt",
        "source.py:1: unused variable 'orphan' (60% confidence)\n",
    );
    fixture.write("source.py", "orphan = 1\n");
    fixture.write("broken.py", "def (\n");
    let output = fixture.replay(&[]);
    assert_eq!(output.status.code(), Some(2));
    let report = fixture.report();
    assert_eq!(report["complete"], false);
    assert!(report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["path"] == "broken.py"));
    assert_success(&fixture.replay(&["--exclude-reference", "broken.py"]));
    assert_eq!(fixture.report()["complete"], true);
}

#[test]
fn malformed_findings_preserve_the_previous_report() {
    let fixture = Fixture::new();
    fixture.write("findings.txt", "source.py:1: unexpected Vulture message\n");
    fixture.write(".forge/reports/dead-code/latest.json", "previous report");
    let output = fixture.replay(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        fs::read_to_string(fixture.0.join(".forge/reports/dead-code/latest.json")).unwrap(),
        "previous report"
    );
}

#[test]
fn output_collisions_are_rejected_without_changing_inputs() {
    let fixture = Fixture::new();
    let findings = "source.py:1: unused variable 'orphan' (60% confidence)\n";
    fixture.write("findings.json", findings);
    let output = fixture
        .command()
        .args([
            "--vulture-output",
            "findings.json",
            "--output",
            "./findings.json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("collides"));
    assert_eq!(
        fs::read_to_string(fixture.0.join("findings.json")).unwrap(),
        findings
    );
    fixture.write("source.py", "orphan = 1\n");
    let output = fixture
        .command()
        .args(["--vulture-output", "findings.json", "--output", "source.py"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        fs::read_to_string(fixture.0.join("source.py")).unwrap(),
        "orphan = 1\n"
    );
}

#[cfg(unix)]
fn executable(fixture: &Fixture, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    fixture.write("vulture-stub", script);
    let path = fixture.0.join("vulture-stub");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn vulture_findings_exit_code_is_accepted_and_invocation_is_recorded() {
    let fixture = Fixture::new();
    fixture.write("source.py", "orphan = 1\n");
    let vulture = executable(&fixture, "#!/bin/sh\nprintf \"source.py:1: unused variable 'orphan' (60%% confidence)\\n\"\nexit 3\n");
    let output = fixture
        .command()
        .arg("--vulture")
        .arg(vulture)
        .args(["--path", "source.py"])
        .output()
        .unwrap();
    assert_success(&output);
    let report = fixture.report();
    assert_eq!(report["invocation"]["vulture_exit_code"], 3);
    assert!(report["invocation"]["command"]
        .as_array()
        .unwrap()
        .contains(&Value::String("--sort-by-size".into())));
    assert_eq!(report["findings"][0]["verdict"], "DEAD");
}

#[cfg(unix)]
#[test]
fn vulture_errors_do_not_publish_an_empty_clean_report() {
    let fixture = Fixture::new();
    let vulture = executable(
        &fixture,
        "#!/bin/sh\necho 'could not parse source.py' >&2\nexit 1\n",
    );
    let output = fixture
        .command()
        .arg("--vulture")
        .arg(vulture)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not parse source.py"));
    assert!(!fixture
        .0
        .join(".forge/reports/dead-code/latest.json")
        .exists());
}

#[test]
fn help_and_argument_validation_do_not_require_vulture() {
    let fixture = Fixture::new();
    let output = fixture.command().arg("--help").output().unwrap();
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("--vulture-output"));
    let output = fixture
        .command()
        .args(["--min-confidence", "101"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!Path::new(&fixture.0.join(".forge/reports/dead-code/latest.json")).exists());
}
