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
use std::{fs, os::unix::fs::PermissionsExt};
#[test]
#[ignore = "requires explicitly provisioned FORGE_TEST_CARGO_BIN, FORGE_TEST_LLVM_COV and FORGE_TEST_LLVM_PROFDATA"]
fn actual_rust_instrumentation_exports_candidate_source_lcov() {
    let manifest = b"[package]\nname='native-fixture'\nversion='0.1.0'\nedition='2021'\n";
    let lock = b"version = 4\n[[package]]\nname = 'native-fixture'\nversion = '0.1.0'\n";
    let src=b"pub fn answer(v: bool) -> u8 {\n if v { 1 } else { 2 }\n}\n#[test] fn test_answer() { assert_eq!(answer(true),1); }\n";
    let f = Private::new(
        &[],
        &[
            ("Cargo.toml", manifest),
            ("Cargo.lock", lock),
            ("src/lib.rs", src),
        ],
    );
    let private = f.case.fixture.root.join("coverage-runtime");
    let tmp = private.join("tmp");
    fs::create_dir_all(&tmp).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).unwrap();
    let cov = std::env::var("FORGE_TEST_LLVM_COV").expect("explicit LLVM cov");
    let prof = std::env::var("FORGE_TEST_LLVM_PROFDATA").expect("explicit LLVM profdata");
    let options = [
        "--manifest",
        "Cargo.toml",
        "--bind",
        "src/lib.rs",
        "--coverage",
        "--llvm-cov",
        cov.as_str(),
        "--llvm-profdata",
        prof.as_str(),
        "--execute",
    ];
    let mut command = f.tool_command_options("cargo-test", &options, &[]);
    command
        .env("TMPDIR", &tmp)
        .env(
            "FORGE_CARGO_BIN",
            std::env::var_os("FORGE_TEST_CARGO_BIN").expect("explicit Cargo bin"),
        )
        .env("FORGE_CARGO_EXTRA_PATH", "/usr/bin");
    let before = f.case.source_state();
    let output = run(command);
    assert_code(&output, 0);
    let coverage = fs::read_to_string(private.join("rust-coverage.lcov")).unwrap();
    assert!(
        coverage.contains("SF:") && coverage.contains("src/lib.rs") && coverage.contains("DA:")
    );
    assert_eq!(f.case.source_state(), before);
    assert!(!f.snapshot.path().join("target").exists());
}
