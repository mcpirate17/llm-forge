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
const MANIFEST: &[u8] = b"[package]\nname='native-fixture'\nversion='0.1.0'\nedition='2021'\n";
const LOCK: &[u8] = b"version = 4\n[[package]]\nname = 'native-fixture'\nversion = '0.1.0'\n";
fn fixture(source: &[u8]) -> Private {
    Private::new(
        &[],
        &[
            ("Cargo.toml", MANIFEST),
            ("Cargo.lock", LOCK),
            ("src/lib.rs", source),
        ],
    )
}
#[test]
fn captured_preflight_binds_private_tree_and_selected_files() {
    let f = fixture(b"#[test] fn works() {}\n");
    let options = ["--manifest", "Cargo.toml", "--bind", "src/lib.rs"];
    let output = run(f.tool_command_options("cargo-test", &options, &[]));
    assert_code(&output, 0);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["executed"], false);
    assert_eq!(report["preimage_tree"], f.tree);
    assert_eq!(report["coverage_verified"], false);
    let missing = run(f.tool_command_options(
        "cargo-test",
        &["--manifest", "Cargo.toml", "--bind", "src/missing.rs"],
        &[],
    ));
    assert!(!missing.status.success());
    let traversal =
        run(f.tool_command_options("cargo-test", &["--manifest", "../Cargo.toml"], &[]));
    assert!(!traversal.status.success());
    fs::write(f.snapshot.path().join("src/lib.rs"), b"drifted").unwrap();
    let drifted = run(f.tool_command_options("cargo-test", &options, &[]));
    assert!(!drifted.status.success());
}
#[test]
#[ignore = "requires explicitly provisioned FORGE_TEST_CARGO_BIN native toolchain"]
fn real_native_cargo_reports_zero_and_nonzero_without_host_target_writes() {
    for (code, source) in [
        (0, b"#[test] fn works() {}\n".as_slice()),
        (101, b"#[test] fn fails() { panic!() }\n".as_slice()),
    ] {
        let f = fixture(source);
        let tmp = f.case.fixture.root.join("cargo-temp");
        fs::create_dir(&tmp).unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = f.tool_command_options(
            "cargo-test",
            &[
                "--manifest",
                "Cargo.toml",
                "--bind",
                "src/lib.rs",
                "--execute",
            ],
            &[],
        );
        command
            .env("TMPDIR", &tmp)
            .env(
                "FORGE_CARGO_BIN",
                std::env::var_os("FORGE_TEST_CARGO_BIN").expect("native toolchain required"),
            )
            .env("FORGE_CARGO_EXTRA_PATH", "/usr/bin");
        let before = f.case.source_state();
        let output = run(command);
        assert_code(&output, code);
        assert_eq!(f.case.source_state(), before);
        assert!(!f.snapshot.path().join("target").exists());
        assert_eq!(fs::read_dir(tmp).unwrap().count(), 0);
    }
}
