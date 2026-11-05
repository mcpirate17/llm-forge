//! Exercise recipe path/error semantics without running a mutation engine.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for name in ["Makefile", "conductor.mk"] {
            fs::copy(source.join(name), directory.path().join(name)).unwrap();
        }
        let forge = directory.path().join("forge");
        fs::write(&forge, r#"#!/bin/sh
if [ "$1" = project ]; then
    printf '%s\n' resolve >> calls
    test "$2" = --host && test "$3" = "$PWD" && test "$4" = path && test "$5" = mutation-receipt-root || exit 21
    if [ "$RESOLVE_FAIL" = 1 ]; then echo 'invalid receipt configuration' >&2; exit 23; fi
    printf '%s\n' 'relocated/evidence'
else
    printf '%s\n' "$@" >> calls
    if [ "$RUN_FAIL" = 1 ]; then exit 24; fi
fi
"#).unwrap();
        fs::set_permissions(forge, fs::Permissions::from_mode(0o700)).unwrap();
        Self(directory)
    }

    fn make(&self, target: &str, args: &[&str]) -> Output {
        Command::new("make")
            .current_dir(self.0.path())
            .env_remove("MAKEFLAGS")
            .env_remove("MUTATION_RECEIPT")
            .env_remove("MUTATION_RECEIPT_ITERATIONS")
            .env_remove("RESOLVE_FAIL")
            .env_remove("RUN_FAIL")
            .args([
                "--no-print-directory",
                target,
                "FORGE=./forge",
                "MUTATION_CAMPAIGN=lanes/example.json",
                "RUN_STAMP=fixed",
            ])
            .args(args)
            .output()
            .unwrap()
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.0.path().join("calls")).unwrap()
    }
}

#[test]
fn runner_resolves_once_and_preserves_each_override() {
    for (args, receipt, resolve) in [
        (
            vec![],
            "relocated/evidence/.iterations/example_fixed.json",
            true,
        ),
        (
            vec!["MUTATION_RECEIPT=custom/root"],
            "custom/root/.iterations/example_fixed.json",
            false,
        ),
        (
            vec!["MUTATION_RECEIPT_ITERATIONS=custom/iterations"],
            "custom/iterations/example_fixed.json",
            true,
        ),
        (
            vec![
                "MUTATION_RECEIPT=custom/root",
                "MUTATION_RECEIPT_ITERATIONS=custom/iterations",
            ],
            "custom/iterations/example_fixed.json",
            false,
        ),
    ] {
        let fixture = Fixture::new();
        let output = fixture.make("mutation-engine-run", &args);
        assert!(output.status.success(), "{output:?}");
        let calls = fixture.calls();
        assert_eq!(
            calls.lines().filter(|line| *line == "resolve").count(),
            usize::from(resolve)
        );
        assert!(
            calls.contains(&format!("--receipt\n{receipt}\n")),
            "{calls}"
        );
        // Directory creation belongs to the native runner after confinement.
        assert!(!fixture.0.path().join(receipt).parent().unwrap().exists());
    }
}

#[test]
fn resolver_and_engine_errors_stop_recipe_without_success_message() {
    for (variable, error) in [
        ("RESOLVE_FAIL=1", "invalid receipt configuration"),
        ("RUN_FAIL=1", "Error 24"),
    ] {
        let fixture = Fixture::new();
        let output = fixture.make("mutation-engine-run", &[variable]);
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{output:?}"
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("iteration receipt:"));
        if variable.starts_with("RESOLVE") {
            assert_eq!(fixture.calls(), "resolve\n");
        }
        assert!(!fixture.0.path().join("relocated").exists());
    }
}

#[test]
fn rejected_paths_cannot_create_directories_before_native_validation() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.0.path().join("linked")).unwrap();
    let absolute = outside.path().join("absolute");
    let traversal = format!(
        "../{}/traversal",
        outside.path().file_name().unwrap().to_str().unwrap()
    );
    for iterations in [absolute.to_str().unwrap(), &traversal, "linked/symlink"] {
        let output = fixture.make(
            "mutation-engine-run",
            &[
                "RUN_FAIL=1",
                &format!("MUTATION_RECEIPT_ITERATIONS={iterations}"),
            ],
        );
        assert!(!output.status.success(), "{output:?}");
    }
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn promotion_uses_resolved_root_and_iteration_override() {
    let fixture = Fixture::new();
    let iterations = fixture.0.path().join("custom/iterations");
    fs::create_dir_all(&iterations).unwrap();
    fs::write(iterations.join("example_fixed.json"), "sealed receipt").unwrap();
    let output = fixture.make(
        "mutation-receipt-promote",
        &["MUTATION_RECEIPT_ITERATIONS=custom/iterations"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.calls(), "resolve\n");
    assert_eq!(
        fs::read_to_string(
            fixture
                .0
                .path()
                .join("relocated/evidence/example_fixed.json")
        )
        .unwrap(),
        "sealed receipt"
    );
}
