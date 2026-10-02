//! Numerical evidence, identity drift and dirty preview contracts.
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn fixture() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "forge-performance-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("src.rs"), "source\n").unwrap();
    fs::write(path.join("input.txt"), "workload\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["add", "src.rs", "input.txt"],
        vec!["commit", "-qm", "base"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&path)
            .status()
            .unwrap()
            .success());
    }
    path
}
fn forge(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}
fn benchmark(root: &Path, command: &str, output: &str) -> Output {
    forge(
        root,
        &[
            "performance",
            "benchmark",
            "--command",
            command,
            "--source",
            "src.rs",
            "--input",
            "input.txt",
            "--samples",
            "5",
            "--warmups",
            "1",
            "--timeout-seconds",
            "2",
            "--output",
            output,
        ],
    )
}

#[test]
fn receipts_bind_samples_and_pair_export_enforces_policy_budget() {
    let root = fixture();
    let run = benchmark(&root, "printf measured", "baseline.json");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let receipt: Value =
        serde_json::from_slice(&fs::read(root.join("baseline.json")).unwrap()).unwrap();
    assert_eq!(receipt["samples"].as_array().unwrap().len(), 5);
    assert!(receipt["summary"]["max_rss_bytes"].as_u64().unwrap() > 0);
    let compare = forge(
        &root,
        &[
            "performance",
            "compare",
            "--baseline",
            "baseline.json",
            "--current",
            "baseline.json",
        ],
    );
    assert!(
        compare.status.success(),
        "{}",
        String::from_utf8_lossy(&compare.stderr)
    );
    let evidence = forge(
        &root,
        &[
            "performance",
            "compare",
            "--baseline",
            "baseline.json",
            "--current",
            "baseline.json",
            "--output",
            "paired-evidence.json",
        ],
    );
    assert!(
        evidence.status.success(),
        "{}",
        String::from_utf8_lossy(&evidence.stderr)
    );
    let pair: Value =
        serde_json::from_slice(&fs::read(root.join("paired-evidence.json")).unwrap()).unwrap();
    assert_eq!(pair["schema"], "forge.performance-evidence.v1");
    assert!(pair["baseline"]["receipt_sha256"].is_string());
    assert!(!forge(
        &root,
        &[
            "performance",
            "compare",
            "--baseline",
            "baseline.json",
            "--current",
            "baseline.json",
            "--max-regression-percent",
            "20",
            "--output",
            "loose-evidence.json"
        ]
    )
    .status
    .success());
    assert!(!root.join("loose-evidence.json").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn comparison_rejects_tampered_metrics_and_unmatched_inputs() {
    let root = fixture();
    assert!(benchmark(&root, "printf measured", "baseline.json")
        .status
        .success());
    let mut changed: Value =
        serde_json::from_slice(&fs::read(root.join("baseline.json")).unwrap()).unwrap();
    changed["summary"]["p50_ms"] = serde_json::json!(0.001);
    fs::write(
        root.join("tampered.json"),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    assert!(!forge(
        &root,
        &[
            "performance",
            "compare",
            "--baseline",
            "baseline.json",
            "--current",
            "tampered.json"
        ]
    )
    .status
    .success());
    fs::write(root.join("input.txt"), "different workload\n").unwrap();
    assert!(benchmark(&root, "printf measured", "current.json")
        .status
        .success());
    assert!(!forge(
        &root,
        &[
            "performance",
            "compare",
            "--baseline",
            "baseline.json",
            "--current",
            "current.json"
        ]
    )
    .status
    .success());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_workload_source_drift_and_overwrites_issue_no_new_receipt() {
    let root = fixture();
    assert!(!benchmark(&root, "exit 2", "failed.json").status.success());
    assert!(!root.join("failed.json").exists());
    assert!(!benchmark(&root, "printf changed > src.rs", "drift.json")
        .status
        .success());
    assert!(!root.join("drift.json").exists());
    assert!(benchmark(&root, "true", "once.json").status.success());
    let bytes = fs::read(root.join("once.json")).unwrap();
    assert!(!benchmark(&root, "true", "once.json").status.success());
    assert_eq!(bytes, fs::read(root.join("once.json")).unwrap());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dirty_preview_only_plans_affected_targets_and_does_not_create_gate_state() {
    let root = fixture();
    fs::create_dir_all(root.join("native/forge/src")).unwrap();
    fs::write(root.join("native/forge/src/new.rs"), "fn changed() {}\n").unwrap();
    let run = forge(&root, &["preview", "--path", "native/forge/src/new.rs"]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let plan: Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(plan["advisory"], true);
    assert_eq!(plan["targets"][0]["crate_name"], "forge");
    assert!(!root.join(".git/forge-checks").exists());
    assert!(!root.join(".git/forge-previews").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unavailable_requested_profiler_fails_before_workload() {
    let root = fixture();
    let run = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args([
            "performance",
            "profile",
            "--tool",
            "py-spy",
            "--output",
            "out.svg",
            "--",
            "python",
            "program.py",
        ])
        .current_dir(&root)
        .env("PATH", &root)
        .output()
        .unwrap();
    assert!(!run.status.success());
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("optional profiler py-spy is unavailable")
    );
    assert!(!root.join("out.svg").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn executing_dirty_preview_reuses_exact_configuration_and_invalidates_changed_bytes() {
    let root = fixture();
    fs::create_dir_all(root.join("native/forge/src")).unwrap();
    fs::write(
        root.join("native/forge/Cargo.toml"),
        "[package]\nname='forge'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    let source = root.join("native/forge/src/main.rs");
    fs::write(
        &source,
        "fn main() {}\n#[test] fn preview_smoke() { assert_eq!(2+2,4); }\n",
    )
    .unwrap();
    assert!(Command::new("cargo")
        .args([
            "generate-lockfile",
            "--offline",
            "--manifest-path",
            "native/forge/Cargo.toml"
        ])
        .current_dir(&root)
        .status()
        .unwrap()
        .success());
    let run = || {
        let result = forge(
            &root,
            &[
                "preview",
                "--path",
                "native/forge/src/main.rs",
                "--execute",
                "--filter",
                "preview_smoke",
            ],
        );
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        serde_json::from_slice::<Value>(&fs::read(report["receipt"].as_str().unwrap()).unwrap())
            .unwrap()
    };
    let first = run();
    assert_eq!(first["records"][0]["build_reused"], false);
    assert_eq!(run()["records"][0]["build_reused"], true);
    fs::write(
        &source,
        "fn main() {}\n#[test] fn preview_smoke() { assert_eq!(3+3,6); }\n",
    )
    .unwrap();
    let changed = run();
    assert_eq!(changed["records"][0]["build_reused"], false);
    assert_ne!(
        first["records"][0]["source_sha256"],
        changed["records"][0]["source_sha256"]
    );
    assert!(!root.join(".git/forge-checks").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cargo_flamegraph_receives_its_required_literal_subcommand() {
    use std::os::unix::fs::PermissionsExt;
    let root = fixture();
    let tools = root.join("tools");
    fs::create_dir_all(&tools).unwrap();
    let profiler = tools.join("cargo-flamegraph");
    fs::write(&profiler, "#!/bin/sh\nset -eu\ntest \"$1\" = flamegraph\ntest \"$2\" = --output\ntest \"$4\" = --bin\ntest \"$5\" = forge\nprintf '<svg/>' > \"$3\"\n").unwrap();
    fs::set_permissions(&profiler, fs::Permissions::from_mode(0o755)).unwrap();
    let perf = tools.join("perf");
    fs::write(&perf, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&perf, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", tools.display(), std::env::var("PATH").unwrap());
    let run = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args([
            "performance",
            "profile",
            "--tool",
            "cargo-flamegraph",
            "--output",
            "profile.svg",
            "--",
            "--bin",
            "forge",
        ])
        .current_dir(&root)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.join("profile.svg")).unwrap(),
        "<svg/>"
    );
    fs::remove_dir_all(root).unwrap();
}
