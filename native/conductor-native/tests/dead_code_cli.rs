#![cfg(feature = "source-analysis")]

use conductor_native::dead_code::AuditReport;
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

    fn audit(&self, names: &[&str]) -> AuditReport {
        let findings = names
            .iter()
            .map(|name| format!("subject.py:1: unused function '{name}' (60% confidence)\n"))
            .collect::<String>();
        self.write("findings.txt", &findings);
        let output = self.replay(&[]);
        let report: AuditReport = serde_json::from_value(self.report()).unwrap();
        assert_eq!(
            output.status.code(),
            Some(if report.complete { 0 } else { 2 })
        );
        report
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
fn dead_code_rust_syntax_distinguishes_production_tests_and_documentation() {
    let fixture = Fixture::new();
    fixture.write(
        "native.rs",
        r##"// comment_only
/// docs_only
fn bridge() {
module.getattr("lookup");
module.getattr("\x65scaped");
module.getattr(r#"raw_name"#);
target_ident();
py.run(c"c_string", None, None);
quote! { module.getattr("macro_name"); b"bytes_name"; }
}
#[cfg(test)] mod contracts { fn helper() { nested_test(); } }
#[test] fn standalone() { direct_test(); }
#[cfg(all(test, feature="enabled"))] fn gated() { gated_test(); }
#[cfg(any(test, feature="prod"))] fn shared() { possible_prod(); }
#[cfg(not(test))] fn prod() { non_test_prod(); }
#[cfg(feature="test")] fn flag() { feature_prod(); }
#[cfg(test="custom")] fn custom() { custom_prod(); }
#[cfg(all(not(not(test)), feature="enabled"))] fn double() { double_test(); }
#[doc = "attribute_doc"] fn documented() {}
#[pyfunction(name="attribute_name")] fn renamed() {}
"##,
    );
    fixture.write("test_native.rs", "fn check() { file_test(); }");
    let cases = [
        ("comment_only", "DEAD"),
        ("docs_only", "DOC-ONLY"),
        ("attribute_doc", "DOC-ONLY"),
        ("lookup", "ALIVE"),
        ("escaped", "ALIVE"),
        ("raw_name", "ALIVE"),
        ("target_ident", "ALIVE"),
        ("c_string", "ALIVE"),
        ("macro_name", "ALIVE"),
        ("bytes_name", "ALIVE"),
        ("nested_test", "TEST-ONLY"),
        ("direct_test", "TEST-ONLY"),
        ("gated_test", "TEST-ONLY"),
        ("double_test", "TEST-ONLY"),
        ("file_test", "TEST-ONLY"),
        ("possible_prod", "ALIVE"),
        ("non_test_prod", "ALIVE"),
        ("feature_prod", "ALIVE"),
        ("custom_prod", "ALIVE"),
        ("attribute_name", "ALIVE"),
    ];
    let report = fixture.audit(&cases.iter().map(|case| case.0).collect::<Vec<_>>());
    assert!(report.complete, "{:?}", report.diagnostics);
    for (row, (name, verdict)) in report.findings.iter().zip(cases) {
        assert_eq!(row.verdict, verdict, "{name}");
    }
    assert_eq!(
        report.findings[3].prod,
        [("native.rs".into(), 4, "rust-string".into())]
    );
    assert_eq!(
        report.findings[1].docs,
        [("native.rs".into(), 2, "rust-doc".into())]
    );
    assert_eq!(report.findings[10].tests[0].2, "rust-test-name");
}

#[test]
fn dead_code_native_suffixes_remain_explicit_lexical_evidence() {
    let fixture = Fixture::new();
    let extensions = [
        "c", "cc", "cpp", "cxx", "h", "hh", "hpp", "hxx", "cu", "cuh",
    ];
    for extension in extensions {
        fixture.write(&format!("bridge.{extension}"), "module.attr(\"target\");");
    }
    let report = fixture.audit(&["target"]);
    assert!(report.complete);
    assert_eq!(report.scanned_files, extensions.len());
    assert_eq!(report.findings[0].verdict, "ALIVE");
    assert_eq!(report.findings[0].prod.len(), extensions.len());
    assert!(report.findings[0]
        .prod
        .iter()
        .all(|hit| hit.1 == 1 && hit.2 == "text"));
}

#[test]
fn dead_code_rust_parse_failure_is_incomplete() {
    let fixture = Fixture::new();
    fixture.write("broken.rs", "fn broken( { target(); }");
    let report = fixture.audit(&["target"]);
    assert!(!report.complete);
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].path, "broken.rs");
    assert!(report.diagnostics[0]
        .message
        .starts_with("Rust parse failed:"));
}

#[test]
fn rust_file_and_statement_test_contexts_do_not_leak_into_production() {
    let fixture = Fixture::new();
    fixture.write(
        "crate_tests.rs",
        "#![cfg(test)]\nfn helper() { crate_test(); }",
    );
    fixture.write(
        "native.rs",
        r#"fn main() {
    #[cfg(test)] { module.getattr("block_test"); }
    #[cfg(test)] let value = local_test();
    #[cfg(test)] record!("macro_test");
    match 1 { #[cfg(test)] 1 => arm_test(), _ => normal() }
    #[cfg(any(test, feature="prod"))] { possible_prod(); }
    after_test();
}
impl Example { #[cfg(test)] const NAME: &str = "assoc_test"; }
trait ExampleTrait { #[cfg(test)] fn helper() { trait_test(); } }
extern "C" { #[cfg(test)] fn foreign_test(); }
fn parameter(#[cfg(test)] pattern_test: bool) {}
struct S { #[cfg(test)] extra: &'static str, normal: () }
fn build() { let _ = S { #[cfg(test)] extra: "field_value_test", normal: () }; }
fn destructure(value: S) { let S { #[cfg(test)] extra: field_pattern_test, .. } = value; }
struct Generic<#[cfg(test)] generic_test>;
"#,
    );
    let report = fixture.audit(&[
        "crate_test",
        "block_test",
        "local_test",
        "macro_test",
        "arm_test",
        "assoc_test",
        "trait_test",
        "foreign_test",
        "pattern_test",
        "field_value_test",
        "field_pattern_test",
        "generic_test",
        "possible_prod",
        "after_test",
    ]);
    assert!(report.complete);
    for row in &report.findings[..12] {
        assert_eq!(row.verdict, "TEST-ONLY", "{}", row.finding.name);
    }
    for row in &report.findings[12..] {
        assert_eq!(row.verdict, "ALIVE", "{}", row.finding.name);
    }
}

#[test]
fn dead_code_preserves_all_verdicts_and_precedence() {
    let fixture = Fixture::new();
    fixture.write("subject.py", "dead()\n");
    fixture.write("prod.py", "alive()\n");
    fixture.write("tests/test_usage.py", "alive()\ntested()\n");
    fixture.write("guide.md", "alive tested documented\n");
    let report = fixture.audit(&["alive", "tested", "documented", "dead"]);
    assert!(report.complete);
    assert_eq!(report.scanned_files, 4);
    assert_eq!(
        report
            .findings
            .iter()
            .map(|row| row.verdict.as_str())
            .collect::<Vec<_>>(),
        ["ALIVE", "TEST-ONLY", "DOC-ONLY", "DEAD"]
    );
    assert_eq!(
        report.findings[0].prod,
        [("prod.py".to_owned(), 1, "name".to_owned())]
    );
    assert!(report.findings[3].prod.is_empty());
}

#[cfg(unix)]
#[test]
fn dead_code_external_file_symlink_is_incomplete() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    outside.write("source.py", "target()\n");
    std::os::unix::fs::symlink(outside.0.join("source.py"), fixture.0.join("linked.py")).unwrap();
    let report = fixture.audit(&["target"]);
    assert!(!report.complete);
    assert!(report.diagnostics[0].message.contains("outside repository"));
}

#[test]
fn dead_code_invalid_python_retains_incomplete_evidence() {
    let fixture = Fixture::new();
    fixture.write("broken.py", "def broken(\n");
    let report = fixture.audit(&["target"]);
    assert!(!report.complete);
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].path, "broken.py");
    assert!(report.diagnostics[0]
        .message
        .contains("Python parse failed"));
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
