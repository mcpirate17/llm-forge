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
fn native_mull_refresh_previews_then_applies_without_an_interpreter_or_git_on_path() {
    let dir = branch_host();
    fs::create_dir(dir.path().join("tests")).unwrap();
    fs::write(
        dir.path().join("src/add.cpp"),
        "int add(int a,int b){return a+b;}\n",
    )
    .unwrap();
    fs::write(dir.path().join("tests/add.cpp"), "int main(){return 0;}\n").unwrap();
    let written = forge(
        dir.path(),
        &[
            "write",
            "cpp",
            "--mull-confined",
            "--jobs",
            "1",
            "--base",
            "main",
            "--only",
            "src/add.cpp",
            "--extra-test",
            "src/add.cpp=tests/add.cpp",
        ],
    );
    assert!(written.status.success(), "{written:?}");
    let result = payload(&written);
    let manifest = result["written"][0].as_str().unwrap();
    let fragment = result["written"][1].as_str().unwrap();
    let old = fs::read(dir.path().join(manifest)).unwrap();
    let registry = fs::read(dir.path().join(fragment)).unwrap();
    fs::write(
        dir.path().join("src/add.cpp"),
        "int add(int a,int b){return a-b;}\n",
    )
    .unwrap();
    let refresh = |apply: bool| {
        let mut command = Command::new(forge_binary::path());
        command
            .env_clear()
            .env("PATH", "")
            .current_dir(dir.path())
            .args(["mutation", "refresh", manifest]);
        if apply {
            command.arg("--apply");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        payload(&output)
    };
    let preview = refresh(false);
    assert_eq!(preview["status"], "PREVIEW");
    assert_eq!(preview["changed"], true);
    assert_eq!(fs::read(dir.path().join(manifest)).unwrap(), old);
    let applied = refresh(true);
    assert_eq!(applied["status"], "APPLIED");
    assert_eq!(
        applied["proposed_manifest_sha256"],
        preview["proposed_manifest_sha256"]
    );
    assert_eq!(applied["evidence_status"], "NOT_EVALUATED");
    let new = fs::read(dir.path().join(manifest)).unwrap();
    assert_ne!(new, old);
    assert_eq!(refresh(true)["status"], "UNCHANGED");
    assert_eq!(fs::read(dir.path().join(manifest)).unwrap(), new);
    assert_eq!(fs::read(dir.path().join(fragment)).unwrap(), registry);
    fs::write(dir.path().join(manifest), b"{}").unwrap();
    let refused = Command::new(forge_binary::path())
        .env_clear()
        .env("PATH", "")
        .current_dir(dir.path())
        .args(["mutation", "refresh", manifest, "--apply"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(4));
    assert_eq!(fs::read(dir.path().join(manifest)).unwrap(), b"{}");
}

fn python_host() -> tempfile::TempDir {
    let dir = branch_host();
    fs::write(
        dir.path().join("sample.py"),
        "def answer():\n    return 7\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("test_sample.py"),
        "from sample import answer\n\ndef test_answer():\n    assert answer() == 7\n",
    )
    .unwrap();
    git(dir.path(), &["add", "sample.py", "test_sample.py"]);
    git(dir.path(), &["commit", "-qm", "python subject"]);
    dir
}

#[test]
fn confined_fest_profile_preserves_scope_and_is_published_by_the_native_writer() {
    let dir = python_host();
    let common = [
        "plan",
        "python",
        "--only",
        "sample.py",
        "--verbose",
        "--python-jobs",
        "1",
    ];
    let plain = forge(dir.path(), &common);
    assert!(plain.status.success(), "{plain:?}");
    let selected = forge(
        dir.path(),
        &[common.as_slice(), &["--fest-confined"]].concat(),
    );
    assert!(selected.status.success(), "{selected:?}");
    let mut manifests = payload(&selected)["manifests"].clone();
    assert_eq!(manifests.as_array().unwrap().len(), 1);
    assert_eq!(
        manifests[0]["generator"]["options"]
            .as_object_mut()
            .unwrap()
            .remove("execution_profile"),
        Some(serde_json::json!("fest-confined-v1"))
    );
    assert!(manifests[0]["generator"]["options"]
        .as_object()
        .unwrap()
        .is_empty());
    manifests[0]["generator"]
        .as_object_mut()
        .unwrap()
        .remove("options");
    assert_eq!(manifests, payload(&plain)["manifests"]);
    assert!(!dir.path().join("campaigns").exists());
    let published = forge(
        dir.path(),
        &["write", "python", "--only", "sample.py", "--fest-confined"],
    );
    assert!(published.status.success(), "{published:?}");
    let path = dir
        .path()
        .join(payload(&published)["written"][0].as_str().unwrap());
    let manifest: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(
        manifest["generator"]["options"]["execution_profile"],
        "fest-confined-v1"
    );
    assert_eq!(
        manifest["generator"]["source"],
        serde_json::json!(["sample.py"])
    );
}

#[test]
fn confined_fest_rejects_other_languages_and_multiple_workers_without_publication() {
    let dir = python_host();
    for args in [
        vec!["write", "rust", "--base", "main", "--fest-confined"],
        vec![
            "write",
            "python",
            "--only",
            "sample.py",
            "--fest-confined",
            "--python-jobs",
            "2",
        ],
    ] {
        let output = forge(dir.path(), &args);
        assert_eq!(output.status.code(), Some(4), "{output:?}");
        assert_eq!(payload(&output)["status"], "REFUSED");
        assert!(!dir.path().join("campaigns").exists());
    }
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

#[test]
fn workspace_profile_is_explicit_and_preserves_generated_scope() {
    let dir = branch_host();
    let plain = forge(dir.path(), &["plan", "rust", "--base", "main", "--verbose"]);
    let workspace = forge(
        dir.path(),
        &[
            "plan",
            "rust",
            "--base",
            "main",
            "--verbose",
            "--cargo-workspace",
        ],
    );
    assert_eq!(plain.status.code(), Some(0), "{plain:?}");
    assert_eq!(workspace.status.code(), Some(0), "{workspace:?}");
    let expected = payload(&plain)["manifests"].clone();
    let mut selected = payload(&workspace)["manifests"].clone();
    let options = selected[0]["generator"]["options"].as_object_mut().unwrap();
    assert_eq!(
        options.remove("execution_profile"),
        Some(serde_json::json!("cargo-workspace-v2"))
    );
    assert_eq!(
        selected, expected,
        "profile must not broaden generated source or test scope"
    );
    assert!(!dir.path().join("campaigns").exists());
}

#[test]
fn workspace_write_publishes_the_verified_profile_in_the_generated_manifest() {
    let dir = branch_host();
    let result = forge(
        dir.path(),
        &["write", "rust", "--base", "main", "--cargo-workspace"],
    );
    assert_eq!(result.status.code(), Some(0), "{result:?}");
    let body = payload(&result);
    let manifest = dir.path().join(body["written"][0].as_str().unwrap());
    let parsed: Value = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    assert_eq!(
        parsed["generator"]["options"]["execution_profile"],
        "cargo-workspace-v2"
    );
    assert_eq!(
        parsed["generator"]["source"],
        serde_json::json!(["src/changed.rs"])
    );
}

#[test]
fn workspace_profile_refuses_python_and_rust_value_declarations() {
    let dir = branch_host();
    for args in [
        vec!["plan", "python", "--base", "main", "--cargo-workspace"],
        vec![
            "write",
            "rust",
            "--base",
            "main",
            "--cargo-workspace",
            "--rust-value-contract",
            "decl.json",
            "--rust-value-contract-sha256",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ],
    ] {
        let result = forge(dir.path(), &args);
        assert_eq!(result.status.code(), Some(4), "{result:?}");
        assert_eq!(payload(&result)["status"], "REFUSED");
        assert!(!dir.path().join("campaigns").exists());
    }
}

#[test]
fn selected_workspace_manifest_preserves_scope_and_binds_the_test_selection() {
    let dir = branch_host();
    let plain = forge(dir.path(), &["plan", "rust", "--base", "main", "--verbose"]);
    assert_eq!(plain.status.code(), Some(0), "{plain:?}");
    let original = payload(&plain)["manifests"][0].clone();
    for target in ["lib", "test:contract"] {
        let result = forge(
            dir.path(),
            &[
                "plan",
                "rust",
                "--base",
                "main",
                "--verbose",
                "--cargo-workspace",
                "--cargo-test-target",
                target,
                "--cargo-test-filter",
                "chosen::smoke",
            ],
        );
        assert_eq!(result.status.code(), Some(0), "{target}: {result:?}");
        let selected = &payload(&result)["manifests"][0];
        let expected_target = if target == "lib" {
            serde_json::json!({"kind":"lib"})
        } else {
            serde_json::json!({"kind":"test","name":"contract"})
        };
        assert_eq!(
            selected["generator"]["options"]["execution_profile"],
            "cargo-workspace-v3"
        );
        assert_eq!(
            selected["generator"]["options"]["workspace_selector"],
            serde_json::json!({"schema_version":1,"target":expected_target,"filter":"chosen::smoke"})
        );
        for field in ["source_sha256", "test_sha256", "survivor_baseline"] {
            assert_eq!(selected[field], original[field], "{target}: {field}");
        }
        assert_eq!(
            selected["generator"]["source"],
            original["generator"]["source"]
        );
        let mut expected_argv = original["test_argv"].as_array().unwrap().clone();
        expected_argv.extend([
            serde_json::json!(if target == "lib" {
                "--lib"
            } else {
                "--test=contract"
            }),
            serde_json::json!("--"),
            serde_json::json!("chosen::smoke"),
        ]);
        assert_eq!(selected["test_argv"], serde_json::json!(expected_argv));
    }
    assert!(!dir.path().join("campaigns").exists());
}

#[test]
fn workspace_selection_rejects_missing_prerequisites_and_option_injection() {
    let dir = branch_host();
    for extra in [
        vec!["--cargo-test-target", "lib"],
        vec!["--cargo-workspace", "--cargo-test-filter", "chosen"],
        vec!["--cargo-workspace", "--cargo-test-target", "test:"],
        vec!["--cargo-workspace", "--cargo-test-target", "bin:forge"],
        vec!["--cargo-workspace", "--cargo-test-target", "test:../escape"],
        vec![
            "--cargo-workspace",
            "--cargo-test-target",
            "lib",
            "--cargo-test-filter=--ignored",
        ],
        vec![
            "--cargo-workspace",
            "--cargo-test-target",
            "lib",
            "--cargo-test-filter",
            "a b",
        ],
    ] {
        let mut args = vec!["write", "rust", "--base", "main"];
        args.extend(extra);
        let result = forge(dir.path(), &args);
        assert!(!result.status.success(), "{args:?}: {result:?}");
        assert!(!dir.path().join("campaigns").exists(), "{args:?}");
    }
}
