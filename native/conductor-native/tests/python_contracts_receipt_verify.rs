#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for four-part target-tree receipt authentication.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyList;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{module, text, AttrPatch, Case};

const MANIFEST: &str = "conductor/mutation_campaigns/campaign.json";
const SOURCE: &[(&str, &[u8])] = &[
    ("src/module.py", b"def add(a, b):\n    return a + b\n"),
    (
        "tests/test_module.py",
        b"def test_add():\n    assert add(1, 2) == 3\n",
    ),
];

fn isolated_case() -> Case {
    let mut case = Case::new();
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
    ] {
        case.remove_env(name);
    }
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    case.set_env("GIT_CONFIG_SYSTEM", "/dev/null");
    case
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("fixture git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn sha256(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

struct Fixture {
    case: Case,
    repo: PathBuf,
    tree_oid: String,
    manifest_bytes: Vec<u8>,
    pins: Value,
}

impl Fixture {
    fn new() -> Self {
        let case = isolated_case();
        let repo = case.mkdir("repo");
        git(&repo, &["init", "--quiet"]);
        git(&repo, &["config", "user.name", "Receipt Verify Test"]);
        git(
            &repo,
            &["config", "user.email", "receipt-verify@example.invalid"],
        );
        git(&repo, &["config", "commit.gpgsign", "false"]);
        let mut pins = serde_json::Map::new();
        for (name, data) in SOURCE {
            pins.insert((*name).to_owned(), json!(sha256(data)));
            let target = repo.join(name);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, data).unwrap();
        }
        let pins = Value::Object(pins);
        let manifest_bytes = serde_json::to_vec_pretty(&json!({
            "schema_version": 1, "campaign_id": "receipt-verify-fixture",
            "source_sha256": pins,
        }))
        .unwrap();
        let manifest_path = repo.join(MANIFEST);
        fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
        fs::write(manifest_path, &manifest_bytes).unwrap();
        git(
            &repo,
            &[
                "add",
                "--",
                "src/module.py",
                "tests/test_module.py",
                MANIFEST,
            ],
        );
        git(&repo, &["commit", "--quiet", "-m", "fixture tree"]);
        let tree_oid = git(&repo, &["rev-parse", "HEAD^{tree}"]);
        Self {
            case,
            repo,
            tree_oid,
            manifest_bytes,
            pins,
        }
    }

    fn receipt(&self) -> PathBuf {
        let payload = json!({
            "schema_version": "llm.mutation-testing.receipt.v3",
            "campaign_id": "receipt-verify-fixture", "status": "PASS",
            "manifest": MANIFEST,
            "manifest_sha256": sha256(&self.manifest_bytes),
            "source_sha256": self.pins,
        });
        self.case.write(
            "receipt.json",
            &serde_json::to_string_pretty(&payload).unwrap(),
        )
    }
}

fn run_main(
    receipt: &Path,
    tree: &str,
    repo: Option<&Path>,
    json_output: bool,
) -> (i32, String, String) {
    Python::attach(|py| {
        let mut args = vec![
            "--receipt".to_owned(),
            receipt.display().to_string(),
            "--tree".to_owned(),
            tree.to_owned(),
        ];
        if let Some(repo) = repo {
            args.extend(["--repo".to_owned(), repo.display().to_string()]);
        }
        if json_output {
            args.push("--json".to_owned());
        }
        let io = module(py, "io");
        let stdout = io.call_method0("StringIO").unwrap();
        let stderr = io.call_method0("StringIO").unwrap();
        let sys = module(py, "sys");
        let _out_patch = AttrPatch::replace(sys.as_any(), "stdout", &stdout);
        let _err_patch = AttrPatch::replace(sys.as_any(), "stderr", &stderr);
        let code: i32 = module(py, "conductor.receipt_verify")
            .getattr("main")
            .unwrap()
            .call1((PyList::new(py, &args).unwrap(),))
            .unwrap()
            .extract()
            .unwrap();
        (
            code,
            text(&stdout.call_method0("getvalue").unwrap()),
            text(&stderr.call_method0("getvalue").unwrap()),
        )
    })
}

#[test]
fn all_four_parts_pass() {
    let fixture = Fixture::new();
    let (code, out, _) = run_main(&fixture.receipt(), "HEAD", Some(&fixture.repo), false);
    assert_eq!(code, 0);
    let lines: Vec<_> = out.trim().lines().collect();
    assert!(lines[0].starts_with("repo_root="));
    assert_eq!(lines[1], format!("tree_oid={}", fixture.tree_oid));
    assert!(lines[2].starts_with("PASS "));
    assert!(lines[2].contains(&format!("tree={}", fixture.tree_oid)));
    assert!(lines[2].contains("pins=2"));
}

#[test]
fn pass_with_raw_tree_oid() {
    let fixture = Fixture::new();
    let (code, out, _) = run_main(
        &fixture.receipt(),
        &fixture.tree_oid,
        Some(&fixture.repo),
        true,
    );
    assert_eq!(code, 0);
    let verdict: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(verdict["status"], "PASS");
    assert_eq!(verdict["tree_oid"], fixture.tree_oid);
    let statuses: serde_json::Map<String, Value> = verdict["checks"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, check)| (name.clone(), check["status"].clone()))
        .collect();
    assert_eq!(
        Value::Object(statuses),
        json!({
            "manifest_blob_in_tree": "PASS", "manifest_sha256": "PASS",
            "source_pins": "PASS", "inventory_digest": "PASS",
        })
    );
}

#[test]
fn json_resolution_lines_go_to_stderr() {
    let fixture = Fixture::new();
    let (code, out, err) = run_main(&fixture.receipt(), "HEAD", Some(&fixture.repo), true);
    assert_eq!(code, 0);
    let _: Value = serde_json::from_str(&out).unwrap();
    assert!(err.contains(&format!("tree_oid={}", fixture.tree_oid)));
}

#[test]
fn refused_on_empty_receipt() {
    let fixture = Fixture::new();
    let stub = fixture.case.root().join("stub-receipt.json");
    fs::write(&stub, b"").unwrap();
    let (code, _, err) = run_main(&stub, &fixture.tree_oid, Some(&fixture.repo), false);
    assert_eq!(code, 4);
    assert!(err.contains("REFUSED") && err.contains("receipt file is empty"));
}

#[test]
fn refused_on_unresolvable_tree() {
    let fixture = Fixture::new();
    let (code, _, err) = run_main(&fixture.receipt(), "deadbeef", Some(&fixture.repo), false);
    assert_eq!(code, 4);
    assert!(err.contains("REFUSED"));
}

#[test]
fn module_entrypoint_subprocess() {
    let fixture = Fixture::new();
    let executable: String = Python::attach(|py| {
        module(py, "sys")
            .getattr("executable")
            .unwrap()
            .extract()
            .unwrap()
    });
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
    let output = Command::new(executable)
        .args(["-m", "conductor.receipt_verify", "--receipt"])
        .arg(fixture.receipt())
        .args(["--tree", "HEAD", "--repo"])
        .arg(&fixture.repo)
        .current_dir(package_root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("CUDA_VISIBLE_DEVICES", "")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out = String::from_utf8(output.stdout).unwrap();
    assert!(out.contains(&format!("tree_oid={}", fixture.tree_oid)));
    assert!(out.contains("\nPASS "));
}

#[test]
fn repo_defaults_to_cwd_discovery() {
    let fixture = Fixture::new();
    let _cwd = fixture.case.chdir("repo/src");
    let (code, out, _) = run_main(&fixture.receipt(), "HEAD", None, false);
    assert_eq!(code, 0);
    assert!(out.contains(&format!("repo_root={}", fixture.repo.display())));
    assert!(out.contains(&format!("tree_oid={}", fixture.tree_oid)));
}
