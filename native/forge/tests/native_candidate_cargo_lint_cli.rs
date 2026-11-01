#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use serde_json::Value;
use std::{
    fs,
    process::{Command, Output},
    time::Duration,
};

struct Case(support::Host);
impl Case {
    fn new() -> Self {
        let host = support::Host::new();
        fs::create_dir(host.0.join("repo")).unwrap();
        fs::create_dir(host.0.join("tmp")).unwrap();
        let case = Self(host);
        case.write(
            "forge.toml",
            "schema_version=1\n[conductor]\ncrate_roster='roster.toml'\n",
        );
        case.roster(&["a", "b"], &[], "");
        case.package("a", "first", "pub fn first() {}\n");
        case.package("b", "second", "pub fn second() {}\n");
        case
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.0 .0.join("repo").join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn roster(&self, crates: &[&str], unstyled: &[&str], extra: &str) {
        self.write("roster.toml", &format!("[crates]\ntested={crates:?}\nlinted={crates:?}\nunstyled={unstyled:?}\n[globs]\nmanifests=['Cargo.toml','*/Cargo.toml']\n{extra}"));
    }
    fn package(&self, directory: &str, name: &str, source: &str) {
        self.write(
            &format!("{directory}/Cargo.toml"),
            &format!("[package]\nname='{name}'\nversion='0.1.0'\nedition='2021'\n"),
        );
        self.write(
            &format!("{directory}/Cargo.lock"),
            &format!("version=4\n[[package]]\nname='{name}'\nversion='0.1.0'\n"),
        );
        self.write(&format!("{directory}/src/lib.rs"), source);
    }
    fn command(&self, mode: &str, execute: bool, files: &[&str]) -> Command {
        let mut command = Command::new(support::forge_binary::path());
        command
            .env_clear()
            .env("PATH", "")
            .env("HOME", &self.0 .0)
            .env("TMPDIR", self.0 .0.join("tmp"))
            .env("FORGE_LEDGER_DISABLE", "1")
            .current_dir(self.0 .0.join("repo"))
            .args(["candidate", "cargo-lint", "--mode", mode]);
        if execute {
            command.arg("--execute");
        }
        command.arg("--").args(files);
        command
    }
    fn invoke(&self, mode: &str, execute: bool, files: &[&str]) -> Output {
        run(self.command(mode, execute, files))
    }
    fn real(&self, mode: &str, files: &[&str]) -> Output {
        let bin =
            std::env::var_os("FORGE_TEST_CARGO_BIN").expect("explicit native toolchain required");
        let mut command = self.command(mode, true, files);
        command
            .env("FORGE_CARGO_BIN", bin)
            .env("FORGE_CARGO_EXTRA_PATH", "/usr/bin")
            .env("RUSTC_WRAPPER", "/must-not-run-wrapper")
            .env("CARGO_BUILD_RUSTC_WRAPPER", "/must-not-run-cargo-wrapper");
        run(command)
    }
    fn clean_runtime(&self) {
        assert_eq!(fs::read_dir(self.0 .0.join("tmp")).unwrap().count(), 0);
        assert!(!self.0 .0.join("repo/target").exists());
        assert!(!self.0 .0.join("repo/.cargo-home").exists());
    }
}
fn run(mut command: Command) -> Output {
    forge::process_probe::run(&mut command, Duration::from_secs(45), 1024 * 1024).unwrap()
}
fn code(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn plan(output: Output) -> Value {
    code(&output, 0);
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn dry_run_is_native_sorted_deduplicated_and_needs_no_tools() {
    let case = Case::new();
    let report = plan(case.invoke(
        "fmt",
        false,
        &["b/src/lib.rs", "a/src/lib.rs", "b/Cargo.toml"],
    ));
    assert_eq!(report["executed"], false);
    assert_eq!(report["roster_path"], "roster.toml");
    assert_eq!(
        report["plan"]["manifests"],
        serde_json::json!(["a/Cargo.toml", "b/Cargo.toml"])
    );
    assert_eq!(report["plan"]["input_owners"].as_array().unwrap().len(), 3);
    case.clean_runtime();
}

#[test]
fn native_config_precedence_and_explicit_roster_override_use_confined_paths() {
    let case = Case::new();
    case.write("pyproject.toml", "invalid legacy TOML [");
    plan(case.invoke("fmt", false, &["a/src/lib.rs"]));
    case.write("forge.toml", "invalid native TOML [");
    let mut command = case.command("fmt", false, &["a/src/lib.rs"]);
    command.env("CONDUCTOR_CRATE_ROSTER", "roster.toml");
    plan(run(command));
    let mut command = case.command("fmt", false, &["a/src/lib.rs"]);
    command.env("CONDUCTOR_CRATE_ROSTER", "../roster.toml");
    assert!(!run(command).status.success());
    case.clean_runtime();
}

#[test]
fn missing_malformed_and_symlinked_rosters_fail_before_toolchain_admission() {
    for mode in ["missing", "malformed", "symlink"] {
        let case = Case::new();
        let roster = case.0 .0.join("repo/roster.toml");
        if mode == "malformed" {
            fs::write(&roster, "[crates\n").unwrap();
        } else {
            fs::remove_file(&roster).unwrap();
            if mode == "symlink" {
                std::os::unix::fs::symlink("a/Cargo.toml", &roster).unwrap();
            }
        }
        let output = case.invoke("fmt", true, &["a/src/lib.rs"]);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("FORGE_CARGO_BIN is required"));
        case.clean_runtime();
    }
}

#[test]
fn orphan_unclassified_and_traversal_are_refused() {
    let case = Case::new();
    case.write("orphan.rs", "");
    for selected in ["orphan.rs", "../escape.rs", "/absolute.rs"] {
        assert!(!case.invoke("fmt", false, &[selected]).status.success());
    }
    case.package("unclassified", "new_crate", "");
    let output = case.invoke("fmt", false, &["a/src/lib.rs"]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("unclassified"));
}

#[test]
fn prerequisite_skip_is_incomplete_but_deliberate_unstyled_exclusion_is_successful() {
    let case = Case::new();
    case.roster(
        &["a", "b"],
        &["b"],
        "[prerequisites.a]\nreason='needs generated archive'\ncommand='operator build only'\n",
    );
    let skipped = case.invoke("clippy", true, &["a/src/lib.rs"]);
    code(&skipped, 2);
    assert!(String::from_utf8_lossy(&skipped.stderr).contains("needs generated archive"));
    code(&case.invoke("fmt", true, &["b/src/lib.rs"]), 0);
    code(&case.invoke("fmt", true, &[]), 0);
    case.clean_runtime();
}

#[test]
fn wrapper_diagnostics_escape_candidate_control_characters_and_bidi() {
    let case = Case::new();
    case.roster(&["a", "b"], &[], "[prerequisites.a]\nreason=\"fake\\u001b[2J\\nforged\\u202e\"\ncommand=\"operator\\tcommand\"\n");
    let output = case.invoke("clippy", true, &["a/src/lib.rs"]);
    code(&output, 2);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1);
    assert!(!stderr.contains(['\u{1b}', '\u{202e}', '\t']));
    assert!(stderr.contains("forged\\u{202e}") && stderr.contains("operator\\tcommand"));
    let directory = "evil\u{1b}[2J\ncrate";
    case.package(directory, "evil_name", "pub fn safe() {}\n");
    let quoted = "\"evil\\u001b[2J\\ncrate\"";
    case.write("roster.toml", &format!("[crates]\ntested=['a','b',{quoted}]\nlinted=['a','b']\nunstyled=[{quoted}]\n[globs]\nmanifests=['*/Cargo.toml']\n"));
    let output = case.invoke("fmt", true, &[&format!("{directory}/src/lib.rs")]);
    code(&output, 0);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1);
    assert!(!stderr.contains('\u{1b}'));
    case.clean_runtime();
}

#[test]
fn spaces_and_leading_dashes_are_literal_file_operands() {
    let case = Case::new();
    case.package("-space crate", "named", "pub fn named() {}\n");
    case.roster(&["a", "b", "-space crate"], &[], "");
    let report = plan(case.invoke("fmt", false, &["-space crate/src/lib.rs"]));
    assert_eq!(
        report["plan"]["manifests"],
        serde_json::json!(["-space crate/Cargo.toml"])
    );
}

#[test]
fn execution_requires_operator_native_toolchain_without_ambient_discovery() {
    let case = Case::new();
    let output = case.invoke("fmt", true, &["a/src/lib.rs"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("FORGE_CARGO_BIN"));
    case.clean_runtime();
}

#[test]
fn virtual_workspace_respects_unstyled_members_and_borrowed_targets_expand_scope() {
    let case = Case::new();
    case.write(
        "Cargo.toml",
        "[workspace]\nmembers=['a','b']\nresolver='2'\n",
    );
    case.roster(&[".", "a", "b"], &["b"], "");
    let report = plan(case.invoke("fmt", false, &["Cargo.toml"]));
    assert_eq!(
        report["plan"]["manifests"],
        serde_json::json!(["a/Cargo.toml"])
    );
    assert_eq!(report["plan"]["skips"][0]["crate_path"], "b");
    assert_eq!(report["plan"]["skips"][0]["kind"], "unstyled");
    case.write(
        "b/Cargo.toml",
        "[package]\nname='second'\nversion='0.1.0'\n[lib]\npath='../a/src/shared.rs'\n",
    );
    case.write("a/src/shared.rs", "pub fn shared() {}\n");
    case.roster(&[".", "a", "b"], &[], "");
    let report = plan(case.invoke("fmt", false, &["a/src/lib.rs"]));
    assert_eq!(
        report["plan"]["manifests"],
        serde_json::json!(["a/Cargo.toml", "b/Cargo.toml"])
    );
    assert_eq!(
        report["plan"]["scope_expansions"][0]["reason"],
        "borrowed-target"
    );
    assert_eq!(
        report["plan"]["input_owners"][0]["manifest"],
        "a/Cargo.toml"
    );
    case.clean_runtime();
}

#[test]
#[ignore = "requires FORGE_TEST_CARGO_BIN pointing to the explicit native Rust toolchain"]
fn real_fmt_handles_virtual_workspace_exclusions_and_borrowed_sources() {
    let case = Case::new();
    case.write(
        "Cargo.toml",
        "[workspace]\nmembers=['a','b']\nresolver='2'\n",
    );
    case.roster(&[".", "a", "b"], &["b"], "");
    case.write("b/src/lib.rs", "pub fn bad( ){}\n");
    code(&case.real("fmt", &["Cargo.toml"]), 0);
    case.write("b/Cargo.toml", "[package]\nname='second'\nversion='0.1.0'\nedition='2021'\n[lib]\npath='../a/src/shared.rs'\n");
    case.write("a/src/shared.rs", "pub fn shared( ){}\n");
    case.roster(&[".", "a", "b"], &[], "");
    let failed = case.real("fmt", &["a/src/lib.rs"]);
    code(&failed, 1);
    assert!(String::from_utf8_lossy(&failed.stdout).contains("shared.rs"));
    case.clean_runtime();
}

#[test]
#[ignore = "requires FORGE_TEST_CARGO_BIN pointing to the explicit native Rust toolchain"]
fn real_fmt_checks_selected_files_ignores_candidate_cargo_config_and_continues_after_failure() {
    let case = Case::new();
    case.write(".cargo/config.toml", "not valid Cargo TOML [");
    code(&case.real("fmt", &["a/src/lib.rs"]), 0);
    case.write(
        "a/src/lib.rs",
        "pub fn first( ){let a=  2;println!(\"{}\",a);}\n",
    );
    case.write(
        "b/src/lib.rs",
        "pub fn second( ){let b=  3;println!(\"{}\",b);}\n",
    );
    let before = fs::read(case.0 .0.join("repo/a/src/lib.rs")).unwrap();
    let output = case.real("fmt", &["a/src/lib.rs", "b/src/lib.rs"]);
    code(&output, 1);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("cargo-fmt: a/Cargo.toml") && err.contains("cargo-fmt: b/Cargo.toml"));
    assert_eq!(
        fs::read(case.0 .0.join("repo/a/src/lib.rs")).unwrap(),
        before
    );
    case.clean_runtime();
}

#[test]
#[ignore = "requires FORGE_TEST_CARGO_BIN pointing to the explicit native Rust toolchain"]
fn real_clippy_is_offline_locked_and_keeps_source_and_host_target_unchanged() {
    let case = Case::new();
    case.write(
        ".cargo/config.toml",
        "[build]\nrustc='/must-not-run'\nrustc-wrapper='/must-not-run-wrapper'\n",
    );
    let lock = case.0 .0.join("repo/a/Cargo.lock");
    let before = fs::read(&lock).unwrap();
    code(&case.real("clippy", &["a/src/lib.rs"]), 0);
    case.write("a/src/lib.rs", "pub fn first() { let needless = 1; }\n");
    let failed = case.real("clippy", &["a/src/lib.rs"]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("unused variable"));
    assert_eq!(fs::read(lock).unwrap(), before);
    case.clean_runtime();
}
