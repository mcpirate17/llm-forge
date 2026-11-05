//! `forge mutation plan|write`: scope selection and publication through the
//! installed executable, on a real git history with no interpreter on PATH.

#[path = "support/forge_binary.rs"]
mod forge_binary;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const CRATE_MANIFEST: &str = "[package]\nname = \"widget\"\nversion = \"0.1.0\"\n";
const PROJECT: &str = "[project]\nname = \"host\"\n\n[tool.conductor]\n\
    mutation_registry = \"campaigns/registry.json\"\n";

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "{args:?}: {output:?}");
}

/// A two-file crate on `main`, then a branch changing only `changed.rs`.
fn branch_host() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    fs::write(root.join("pyproject.toml"), PROJECT).expect("pyproject");
    fs::write(root.join("Cargo.toml"), CRATE_MANIFEST).expect("manifest");
    fs::create_dir(root.join("src")).expect("src");
    fs::write(root.join("src/lib.rs"), "pub mod changed;\npub mod kept;\n").expect("lib");
    fs::write(root.join("src/kept.rs"), "#[test]\nfn kept() {}\n").expect("kept");
    fs::write(root.join("src/changed.rs"), "#[test]\nfn changed() {}\n").expect("changed");
    fs::write(root.join(".gitignore"), "target\n").expect("ignore");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base"]);
    git(root, &["checkout", "-q", "-b", "topic"]);
    fs::write(root.join("src/changed.rs"), "#[test]\nfn renamed() {}\n").expect("edit");
    git(root, &["commit", "-qam", "change"]);
    dir
}

fn forge(root: &Path, args: &[&str]) -> Output {
    Command::new(forge_binary::path())
        .env("PATH", std::env::var("PATH").expect("PATH"))
        .env_remove("CONDUCTOR_MUTATION_REGISTRY")
        .current_dir(root)
        .args(["mutation"])
        .args(args)
        .args(["--owner", "lane", "--day", "20260101"])
        .output()
        .expect("forge runs")
}

fn payload(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("JSON on stdout")
}

#[test]
fn plan_reports_only_the_branch_scope_and_writes_nothing() {
    let dir = branch_host();
    let root = dir.path();

    let output = forge(root, &["plan", "rust", "--base", "main", "--verbose"]);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let body = payload(&output);
    assert_eq!(body["status"], "READY");
    assert_eq!(body["scope"], "changed vs main");
    assert_eq!(body["campaigns"], 1);
    let sources = &body["manifests"][0]["generator"]["source"];
    assert_eq!(sources, &serde_json::json!(["src/changed.rs"]));
    assert!(!root.join("campaigns").exists());
}

#[test]
fn an_unchanged_branch_is_refused_with_the_scope_exit_code() {
    let dir = branch_host();

    let output = forge(dir.path(), &["plan", "rust", "--base", "topic"]);

    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let body = payload(&output);
    assert_eq!(body["status"], "REFUSED");
    assert!(body["error"]
        .as_str()
        .expect("error")
        .starts_with("nothing changed against topic"));
}

#[test]
fn all_files_is_the_explicit_whole_tree_sweep() {
    let dir = branch_host();

    let output = forge(dir.path(), &["plan", "rust", "--all-files", "--verbose"]);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let body = payload(&output);
    assert_eq!(body["scope"], "all-files");
    let sources = body["manifests"][0]["generator"]["source"]
        .as_array()
        .expect("sources")
        .len();
    assert_eq!(sources, 3);
}

#[test]
fn only_and_all_files_cannot_be_combined() {
    let dir = branch_host();

    let output = forge(
        dir.path(),
        &["plan", "rust", "--all-files", "--only", "src/kept.rs"],
    );

    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

#[test]
fn write_publishes_manifest_and_fragment_then_refuses_to_replace_them() {
    let dir = branch_host();
    let root = dir.path();

    let first = forge(root, &["write", "rust", "--base", "main"]);

    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let written = payload(&first)["written"].clone();
    let manifest = written[0].as_str().expect("manifest path");
    let fragment = written[1].as_str().expect("fragment path");
    assert_eq!(manifest, "campaigns/lane__cargo_20260101.json");
    assert_eq!(fragment, "campaigns/registry.d/lane__cargo_20260101.json");
    let text = fs::read_to_string(root.join(manifest)).expect("manifest bytes");
    let parsed: Value = serde_json::from_str(&text).expect("manifest JSON");
    assert_eq!(parsed["mutation_engine"], "cargo-mutants");
    assert!(text.ends_with("}\n"));

    let second = forge(
        root,
        &[
            "write",
            "rust",
            "--only",
            "src/changed.rs",
            "--include-covered",
        ],
    );

    assert_eq!(second.status.code(), Some(4), "{second:?}");
    assert!(payload(&second)["error"]
        .as_str()
        .expect("error")
        .contains("refusing to replace a recorded survivor"));
    assert_eq!(fs::read_to_string(root.join(manifest)).expect("kept"), text);
}
