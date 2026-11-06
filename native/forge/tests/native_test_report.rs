#[allow(dead_code)]
mod support;
use git2::{Repository, Signature, Time};
use serde_json::{json, Value};
use std::{fs, path::Path, process::Output};

fn fixture() -> support::Host {
    let host = support::Host::new();
    let repo = Repository::init(&host.0).unwrap();
    fs::create_dir(host.0.join("pkg")).unwrap();
    let mut index = repo.index().unwrap();
    for (name, text) in [
        ("pkg/__init__.py", ""),
        ("test_broken.py", "import pkg.missing\n"),
        (".gitignore", "audit/\n"),
    ] {
        fs::write(host.0.join(name), text).unwrap();
        index.add_path(Path::new(name)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::new("Fixture", "fixture@example.invalid", &Time::new(0, 0)).unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
    host
}

fn call(host: &support::Host, args: &[&str]) -> Output {
    let mut command = host.command();
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .args(["analyze", "--root", host.0.to_str().unwrap(), "tests"])
        .args(args)
        .env("PATH", "")
        .env("PYO3_PYTHON", "/forbidden-python")
        .env("CONDUCTOR_PYTHON", "/forbidden-python")
        .env("HOME", &host.0)
        .env("XDG_CONFIG_HOME", &host.0)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("CONDUCTOR_NOTES_ROOT")
        .env_remove("CONDUCTOR_MUTATION_REGISTRY")
        .output()
        .unwrap()
}

#[test]
fn native_report_needs_no_path_tools_and_writes_compatible_artifact_only_when_requested() {
    let host = fixture();
    let first = call(&host, &[]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let value: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(
        value["report"]["broken"],
        json!([{"test":"test_broken.py","missing":["pkg.missing"],"last_commit":"1970-01-01"}])
    );
    assert!(!host.0.join("audit").exists());
    let checked = call(&host, &["--check", "--json-out", "audit/dead_tests.json"]);
    assert_eq!(
        checked.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert_eq!(first.stdout, checked.stdout);
    let saved: Value =
        serde_json::from_slice(&fs::read(host.0.join("audit/dead_tests.json")).unwrap()).unwrap();
    assert_eq!(saved, value["report"]);
    fs::write(host.0.join("test_broken.py"), "assert True\n").unwrap();
    assert!(call(&host, &["--check"]).status.success());
}

#[test]
fn refusal_and_output_overflow_publish_neither_json_nor_artifact() {
    let host = fixture();
    for args in [
        vec!["--max-bytes", "512", "--json-out", "audit/refused.json"],
        vec!["--json-out", ".git/report.json"],
        vec!["--json-out", "../escaped.json"],
    ] {
        let output = call(&host, &args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    assert!(!host.0.join("audit").exists());
    fs::write(host.0.join("test_broken.py"), "def broken(:\n").unwrap();
    let output = call(&host, &["--json-out", "audit/refused.json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!host.0.join("audit").exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("test_broken.py"));
}
