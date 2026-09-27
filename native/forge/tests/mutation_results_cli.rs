use serde_json::Value;
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Report(std::path::PathBuf);

impl Report {
    fn new(contents: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "forge-results-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, contents).unwrap();
        Self(path)
    }

    fn parse(&self, adapter: &str, tests: &[&str]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .env("PATH", "")
            .args(["mutation", "results", "--adapter", adapter, "--report"])
            .arg(&self.0);
        for test in tests {
            command.args(["--test", test]);
        }
        command.output().unwrap()
    }
}

impl Drop for Report {
    fn drop(&mut self) {
        fs::remove_file(&self.0).unwrap();
    }
}

#[test]
fn native_results_cover_junit_and_libtest_without_an_interpreter() {
    for (adapter, nodeid, report) in [
        ("pytest-junit", "pkg/test_math.py::test_add", "<testsuite><testcase classname=\"pkg.test_math\" name=\"test_add\" time=\"0.25\"/></testsuite>"),
        ("ctest-junit", "tests/math.c::test_add", "<testsuite><testcase name=\"math.test_add\"><failure/></testcase></testsuite>"),
        ("cargo-libtest", "tests/math.rs::test_add", "test tests::test_add ... ok\n"),
    ] {
        let output = Report::new(report).parse(adapter, &[nodeid]);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(parsed["status"], "COMPLETE");
        assert!(parsed["missing_nodeids"].as_array().unwrap().is_empty());
    }
}

#[test]
fn missing_tests_return_incomplete_json_and_nonzero_status() {
    let output = Report::new("test tests::test_add ... ok\n")
        .parse("cargo-libtest", &["tests/math.rs::test_missing"]);
    assert_eq!(output.status.code(), Some(1));
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["status"], "INCOMPLETE");
    assert_eq!(parsed["missing_nodeids"][0], "tests/math.rs::test_missing");
}

#[test]
fn malformed_xml_and_ambiguous_ids_return_errors_without_result_json() {
    for (report, adapter, tests) in [
        ("<testsuite>", "pytest-junit", vec!["pkg/test_x.py::test_x"]),
        (
            "test test_x ... ok\n",
            "cargo-libtest",
            vec!["a.rs::test_x", "b.rs::test_x"],
        ),
    ] {
        let output = Report::new(report).parse(adapter, &tests);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}
