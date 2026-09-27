#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for cargo-audit toolchain recovery and lockfile selection.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use support::{module, path, text, AttrPatch, Case};

fn rustup(case: &Case, home: &str) -> PathBuf {
    case.write(
        &format!("{home}/.rustup/settings.toml"),
        "default_toolchain = \"stable\"\n",
    );
    case.write(
        &format!("{home}/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/marker"),
        "",
    );
    case.root().join(home).join(".rustup")
}

fn cargo(case: &Case, home: &str) -> PathBuf {
    case.mkdir(&format!("{home}/.cargo/bin"));
    case.root().join(home).join(".cargo")
}

fn audit<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.candidate_review.cargo_audit_files")
}

fn mock_return<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("return_value", value).unwrap();
    module(py, "unittest.mock")
        .getattr("Mock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn login_home(py: Python<'_>, audit: &Bound<'_, PyModule>, home: Option<&Path>) -> AttrPatch {
    let value = match home {
        Some(home) => path(py, home).unbind(),
        None => py.None(),
    };
    let kwargs = PyDict::new(py);
    kwargs.set_item("return_value", value.bind(py)).unwrap();
    let original = audit.getattr("_login_home").unwrap();
    let fake = module(py, "unittest.mock")
        .getattr("create_autospec")
        .unwrap()
        .call((original,), Some(&kwargs))
        .unwrap();
    AttrPatch::replace(audit, "_login_home", &fake)
}

fn toolchain_env(
    py: Python<'_>,
    audit: &Bound<'_, PyModule>,
    entries: &[(&str, &str)],
) -> BTreeMap<String, String> {
    let environ = PyDict::new(py);
    for (key, value) in entries {
        environ.set_item(*key, *value).unwrap();
    }
    audit
        .getattr("rust_toolchain_env")
        .unwrap()
        .call1((environ,))
        .unwrap()
        .extract()
        .unwrap()
}

fn assert_version_run(run: &Bound<'_, PyAny>, expected_rustup: Option<&Path>) {
    assert_eq!(
        run.getattr("call_count").unwrap().extract::<i64>().unwrap(),
        1
    );
    let call = run.getattr("call_args").unwrap();
    let args = call.getattr("args").unwrap();
    assert_eq!(args.len().unwrap(), 1);
    let argv: Vec<String> = args.get_item(0).unwrap().extract().unwrap();
    assert_eq!(argv, ["cargo", "audit", "--version"]);
    let kwargs = call.getattr("kwargs").unwrap();
    assert_eq!(kwargs.len().unwrap(), 2);
    assert!(!kwargs.get_item("check").unwrap().extract::<bool>().unwrap());
    let env = kwargs.get_item("env").unwrap();
    let actual = env.call_method1("get", ("RUSTUP_HOME",)).unwrap();
    match expected_rustup {
        Some(expected) => assert_eq!(text(&actual), expected.to_str().unwrap()),
        None => assert!(actual.is_none()),
    }
}

#[test]
fn explicit_settings_are_never_overridden() {
    let case = Case::new();
    case.mkdir("sandbox");
    rustup(&case, "sandbox");
    cargo(&case, "sandbox");
    Python::attach(|py| {
        let audit = audit(py);
        let _login = login_home(py, &audit, None);
        let home = case.root().join("sandbox");
        let env = toolchain_env(
            py,
            &audit,
            &[
                ("HOME", home.to_str().unwrap()),
                ("RUSTUP_HOME", "/pinned/rustup"),
                ("CARGO_HOME", "/pinned/cargo"),
            ],
        );
        assert_eq!(env["RUSTUP_HOME"], "/pinned/rustup");
        assert_eq!(env["CARGO_HOME"], "/pinned/cargo");
    });
}

#[test]
fn login_home_recovers_what_sandbox_home_hides() {
    let case = Case::new();
    let sandbox = case.mkdir("runtime-home");
    let login = case.mkdir("login");
    let expected_rustup = rustup(&case, "login");
    let expected_cargo = cargo(&case, "login");
    Python::attach(|py| {
        let audit = audit(py);
        let _login = login_home(py, &audit, Some(&login));
        let env = toolchain_env(py, &audit, &[("HOME", sandbox.to_str().unwrap())]);
        assert_eq!(env["RUSTUP_HOME"], expected_rustup.to_str().unwrap());
        assert_eq!(env["CARGO_HOME"], expected_cargo.to_str().unwrap());
    });
}

#[test]
fn sandbox_home_precedes_login_home() {
    let case = Case::new();
    let sandbox = case.mkdir("sandbox");
    let login = case.mkdir("login");
    let expected = rustup(&case, "sandbox");
    let fallback = rustup(&case, "login");
    Python::attach(|py| {
        let audit = audit(py);
        let _login = login_home(py, &audit, Some(&login));
        let env = toolchain_env(py, &audit, &[("HOME", sandbox.to_str().unwrap())]);
        assert_eq!(env["RUSTUP_HOME"], expected.to_str().unwrap());
        assert_ne!(env["RUSTUP_HOME"], fallback.to_str().unwrap());
    });
}

#[test]
fn home_directories_require_their_own_markers() {
    let case = Case::new();
    let sandbox = case.mkdir("sandbox");
    case.mkdir("sandbox/.rustup");
    case.mkdir("sandbox/.cargo");
    let login = case.mkdir("login");
    let expected_rustup = rustup(&case, "login");
    let expected_cargo = cargo(&case, "login");
    Python::attach(|py| {
        let audit = audit(py);
        let _login = login_home(py, &audit, Some(&login));
        let env = toolchain_env(py, &audit, &[("HOME", sandbox.to_str().unwrap())]);
        assert_eq!(env["RUSTUP_HOME"], expected_rustup.to_str().unwrap());
        assert_eq!(env["CARGO_HOME"], expected_cargo.to_str().unwrap());
    });
}

#[test]
fn rustup_shell_without_default_is_rejected() {
    let case = Case::new();
    let sandbox = case.mkdir("sandbox");
    case.mkdir("sandbox/.rustup/toolchains");
    case.write(
        "sandbox/.rustup/settings.toml",
        "version = \"12\"\n\n[overrides]\n",
    );
    let login = case.mkdir("login");
    let expected = rustup(&case, "login");
    Python::attach(|py| {
        let audit = audit(py);
        let _login = login_home(py, &audit, Some(&login));
        let env = toolchain_env(py, &audit, &[("HOME", sandbox.to_str().unwrap())]);
        assert_eq!(env["RUSTUP_HOME"], expected.to_str().unwrap());
    });
}

#[test]
fn named_but_uninstalled_toolchain_is_rejected() {
    let case = Case::new();
    let sandbox = case.mkdir("sandbox");
    case.mkdir("sandbox/.rustup/toolchains");
    case.write(
        "sandbox/.rustup/settings.toml",
        "default_toolchain = \"stable\"\n",
    );
    let login = case.mkdir("login");
    let expected = rustup(&case, "login");
    Python::attach(|py| {
        let audit = audit(py);
        let _login = login_home(py, &audit, Some(&login));
        let env = toolchain_env(py, &audit, &[("HOME", sandbox.to_str().unwrap())]);
        assert_eq!(env["RUSTUP_HOME"], expected.to_str().unwrap());
    });
}

#[test]
fn cargo_home_requires_bin_even_when_rustup_marker_exists() {
    let case = Case::new();
    let login = case.mkdir("login");
    let expected = rustup(&case, "login");
    case.write("login/.cargo/settings.toml", "");
    Python::attach(|py| {
        let audit = audit(py);
        let _login = login_home(py, &audit, Some(&login));
        let absent = case.root().join("absent");
        let env = toolchain_env(py, &audit, &[("HOME", absent.to_str().unwrap())]);
        assert_eq!(env["RUSTUP_HOME"], expected.to_str().unwrap());
        assert!(!env.contains_key("CARGO_HOME"));
    });
}

#[test]
fn host_without_rustup_or_cargo_home_is_unchanged() {
    let case = Case::new();
    Python::attach(|py| {
        let audit = audit(py);
        let _login = login_home(py, &audit, None);
        let absent = case.root().join("absent");
        let env = toolchain_env(py, &audit, &[("HOME", absent.to_str().unwrap())]);
        assert!(!env.contains_key("RUSTUP_HOME"));
        assert!(!env.contains_key("CARGO_HOME"));
    });
}

#[test]
fn missing_home_variable_uses_login_home() {
    let case = Case::new();
    let login = case.mkdir("login");
    let expected = rustup(&case, "login");
    Python::attach(|py| {
        let audit = audit(py);
        let _login = login_home(py, &audit, Some(&login));
        let env = toolchain_env(py, &audit, &[]);
        assert_eq!(env["RUSTUP_HOME"], expected.to_str().unwrap());
    });
}

#[test]
fn blank_and_absent_rustup_settings_both_use_login_home() {
    let case = Case::new();
    let login = case.mkdir("login");
    let expected = rustup(&case, "login");
    Python::attach(|py| {
        let audit = audit(py);
        let _login = login_home(py, &audit, Some(&login));
        let absent = case.root().join("absent");
        for setting in [Some(""), None] {
            let mut entries = vec![("HOME", absent.to_str().unwrap())];
            if let Some(value) = setting {
                entries.push(("RUSTUP_HOME", value));
            }
            let env = toolchain_env(py, &audit, &entries);
            assert_eq!(
                env["RUSTUP_HOME"],
                expected.to_str().unwrap(),
                "{setting:?}"
            );
        }
    });
}

#[test]
fn missing_passwd_entry_returns_no_login_home() {
    let _case = Case::new();
    Python::attach(|py| {
        let audit = audit(py);
        let pwd = audit.getattr("pwd").unwrap();
        let key_error = PyModule::import(py, "builtins")
            .unwrap()
            .getattr("KeyError")
            .unwrap()
            .call1(("no passwd entry",))
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("side_effect", key_error).unwrap();
        let fake = module(py, "unittest.mock")
            .getattr("Mock")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let _passwd = AttrPatch::replace(&pwd, "getpwuid", &fake);
        assert!(audit
            .getattr("_login_home")
            .unwrap()
            .call0()
            .unwrap()
            .is_none());
        assert_eq!(
            fake.getattr("call_count")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            1
        );
        let args = fake.getattr("call_args").unwrap().getattr("args").unwrap();
        assert_eq!(args.len().unwrap(), 1);
        let expected: i64 = module(py, "os")
            .getattr("getuid")
            .unwrap()
            .call0()
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            args.get_item(0).unwrap().extract::<i64>().unwrap(),
            expected
        );
    });
}

#[test]
fn nearest_lockfile_above_changed_file_wins() {
    let case = Case::new();
    case.mkdir("native/crate/src");
    case.write("Cargo.lock", "");
    let expected = case.write("native/crate/Cargo.lock", "");
    Python::attach(|py| {
        let audit = audit(py);
        let kwargs = PyDict::new(py);
        kwargs.set_item("root", path(py, case.root())).unwrap();
        let file = path(py, &case.root().join("native/crate/src/lib.rs"));
        let found = audit
            .getattr("_owning_lockfile")
            .unwrap()
            .call((file,), Some(&kwargs))
            .unwrap();
        assert_eq!(text(&found), expected.to_str().unwrap());
    });
}

#[test]
fn outside_root_owns_no_lockfile() {
    let case = Case::new();
    let root = case.mkdir("repo");
    case.write("repo/Cargo.lock", "");
    let outside = case.mkdir("elsewhere");
    Python::attach(|py| {
        let audit = audit(py);
        let kwargs = PyDict::new(py);
        kwargs.set_item("root", path(py, &root)).unwrap();
        let file = path(py, &outside.join("lib.rs"));
        let found = audit
            .getattr("_owning_lockfile")
            .unwrap()
            .call((file,), Some(&kwargs))
            .unwrap();
        assert!(found.is_none());
    });
}

#[test]
fn version_probe_uses_recovered_toolchain_and_exact_command() {
    let mut case = Case::new();
    rustup(&case, ".");
    let rustup = case.root().join(".rustup");
    let home = case.root().to_str().unwrap().to_owned();
    case.set_env("HOME", &home);
    case.remove_env("RUSTUP_HOME");
    case.remove_env("CARGO_HOME");
    Python::attach(|py| {
        let audit = audit(py);
        let completed = PyModule::import(py, "types")
            .unwrap()
            .getattr("SimpleNamespace")
            .unwrap()
            .call1(())
            .unwrap();
        completed.setattr("returncode", 0).unwrap();
        let run = mock_return(py, &completed);
        let subprocess = audit.getattr("subprocess").unwrap();
        let _run = AttrPatch::replace(&subprocess, "run", &run);
        let result: i64 = audit
            .getattr("main")
            .unwrap()
            .call1((vec!["--version"],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(result, 0);
        assert_version_run(&run, Some(&rustup));
    });
}

#[test]
fn failed_version_probe_reports_missing_rustup_home() {
    let mut case = Case::new();
    let home = case.root().to_str().unwrap().to_owned();
    case.set_env("HOME", &home);
    case.remove_env("RUSTUP_HOME");
    case.remove_env("CARGO_HOME");
    Python::attach(|py| {
        let audit = audit(py);
        let _login = login_home(py, &audit, None);
        let completed = PyModule::import(py, "types")
            .unwrap()
            .getattr("SimpleNamespace")
            .unwrap()
            .call0()
            .unwrap();
        completed.setattr("returncode", 101).unwrap();
        let run = mock_return(py, &completed);
        let subprocess = audit.getattr("subprocess").unwrap();
        let _run = AttrPatch::replace(&subprocess, "run", &run);
        let stderr = module(py, "io")
            .getattr("StringIO")
            .unwrap()
            .call0()
            .unwrap();
        let sys = PyModule::import(py, "sys").unwrap();
        let _stderr = AttrPatch::replace(&sys.into_any(), "stderr", &stderr);
        let result: i64 = audit
            .getattr("main")
            .unwrap()
            .call1((vec!["--version"],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(result, 101);
        assert_version_run(&run, None);
        assert!(text(&stderr.call_method0("getvalue").unwrap()).contains("export RUSTUP_HOME"));
    });
}
