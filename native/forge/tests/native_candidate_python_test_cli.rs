#![cfg(target_os = "linux")]
#[path = "support/candidate_clang_fixture.rs"]
#[allow(dead_code)]
mod candidate_clang_fixture;
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_clang_fixture::{assert_code, run, Private};
#[test]
fn preflight_binds_captured_selected_tests_and_rejects_drift() {
    let f = Private::new(
        &[],
        &[
            ("src/a.py", b"x=1\n"),
            ("tests/test_a.py", b"def test_a(): pass\n"),
        ],
    );
    let options = [
        "--interpreter",
        "/usr/bin/false",
        "--pytest",
        "/usr/bin/false",
        "--test",
        "tests/test_a.py",
        "--bind",
        "src/a.py",
    ];
    assert_code(
        &run(f.tool_command_options("python-test", &options, &[])),
        0,
    );
    let missing_tool = [
        "--interpreter",
        "/definitely/missing/python",
        "--pytest",
        "/usr/bin/false",
        "--test",
        "tests/test_a.py",
    ];
    assert!(
        !run(f.tool_command_options("python-test", &missing_tool, &[]))
            .status
            .success()
    );
    let missing = [
        "--interpreter",
        "/usr/bin/false",
        "--pytest",
        "/usr/bin/false",
        "--test",
        "tests/missing.py",
    ];
    assert!(!run(f.tool_command_options("python-test", &missing, &[]))
        .status
        .success());
    let traversal = [
        "--interpreter",
        "/usr/bin/false",
        "--pytest",
        "/usr/bin/false",
        "--test",
        "../outside.py",
    ];
    assert!(!run(f.tool_command_options("python-test", &traversal, &[]))
        .status
        .success());
    std::fs::write(f.snapshot.path().join("src/a.py"), b"x=2\n").unwrap();
    assert!(!run(f.tool_command_options("python-test", &options, &[]))
        .status
        .success());
}
