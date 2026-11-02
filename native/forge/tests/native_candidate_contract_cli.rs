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
fn contract_preflight_requires_captured_selected_target_and_python() {
    let f = Private::new(
        &[],
        &[
            (
                "native/conductor-native/Cargo.toml",
                b"[package]\nname='fixture'\nversion='0.1.0'\n",
            ),
            (
                "native/forge/Cargo.toml",
                b"[package]\nname='forge'\nversion='0.1.0'\n",
            ),
            (
                "native/slop-core/Cargo.toml",
                b"[package]\nname='slop-core'\nversion='0.1.0'\n",
            ),
            (
                "native/conductor-native/tests/python_contracts_a.rs",
                b"#[test] fn a() {}\n",
            ),
        ],
    );
    let tmp = f.case.fixture.root.join("runtime/tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        tmp.parent().unwrap(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700)).unwrap();
    let options = [
        "--python",
        "/usr/bin/false",
        "--target",
        "python_contracts_a",
    ];
    let mut command = f.tool_command_options("contract-test", &options, &[]);
    command.env("TMPDIR", &tmp);
    assert_code(&run(command), 0);
    let missing = [
        "--python",
        "/usr/bin/false",
        "--target",
        "python_contracts_missing",
    ];
    let mut command = f.tool_command_options("contract-test", &missing, &[]);
    command.env("TMPDIR", &tmp);
    assert!(!run(command).status.success());
    std::fs::write(
        f.snapshot
            .path()
            .join("native/conductor-native/tests/python_contracts_a.rs"),
        b"drifted",
    )
    .unwrap();
    let mut command = f.tool_command_options("contract-test", &options, &[]);
    command.env("TMPDIR", &tmp);
    assert!(!run(command).status.success());
}
#[test]
fn unlisted_contract_does_not_require_absent_slop_but_known_consumer_does() {
    for (target, expected) in [
        ("python_contracts_a", true),
        ("python_contracts_equivalence_probe", false),
    ] {
        let test = format!("native/conductor-native/tests/{target}.rs");
        let files = [
            (
                "native/conductor-native/Cargo.toml",
                b"[package]\nname='fixture'\nversion='0.1.0'\n".as_slice(),
            ),
            (
                "native/forge/Cargo.toml",
                b"[package]\nname='forge'\nversion='0.1.0'\n".as_slice(),
            ),
            (test.as_str(), b"#[test] fn a() {}\n".as_slice()),
        ];
        let fixture = Private::new(&[], &files);
        let tmp = fixture.case.fixture.root.join("runtime/tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            tmp.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700)).unwrap();
        let args = ["--python", "/usr/bin/false", "--target", target];
        let mut command = fixture.tool_command_options("contract-test", &args, &[]);
        command.env("TMPDIR", &tmp);
        let result = run(command);
        assert_eq!(
            result.status.success(),
            expected,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
