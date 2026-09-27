#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the Python registry-array migration boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{assert_error, module, path, Case};

const CAMPAIGNS: &str = "conductor/mutation_campaigns";

fn canonical_patterns() -> Vec<String> {
    Python::attach(|py| {
        module(py, "conductor.mutation_coverage")
            .getattr("CANONICAL_TEST_PATTERNS")
            .unwrap()
            .extract()
            .unwrap()
    })
}

fn registry(repo: &Path, campaigns: Value) -> PathBuf {
    let destination = repo.join(CAMPAIGNS).join("registry.json");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let payload = json!({
        "schema_version": 1,
        "enforcement": "changed_tests",
        "test_patterns": canonical_patterns(),
        "receipt_directories": ["conductor/mutation_campaigns/receipts"],
        "campaigns": campaigns,
    });
    fs::write(
        &destination,
        format!("{}\n", serde_json::to_string_pretty(&payload).unwrap()),
    )
    .unwrap();
    destination
}

fn split(repo: &Path) -> PyResult<Vec<String>> {
    Python::attach(|py| {
        module(py, "conductor.mutation_registry_split")
            .getattr("split_registry_array")
            .unwrap()
            .call1((path(py, repo),))?
            .extract()
    })
}

fn loaded_manifests(registry: &Path, repo: &Path) -> Vec<String> {
    Python::attach(|py| {
        let loaded = module(py, "conductor.mutation_campaign_model")
            .getattr("_load_registry")
            .unwrap()
            .call1((path(py, registry), path(py, repo)))
            .unwrap();
        loaded
            .get_item("campaigns")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|row| {
                row.unwrap()
                    .get_item("manifest")
                    .unwrap()
                    .extract()
                    .unwrap()
            })
            .collect()
    })
}

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, current: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, found);
            } else if path.is_file() {
                found.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut found = BTreeMap::new();
    visit(root, root, &mut found);
    found
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn register(repo: &Path, campaign: &str) {
    let directory = repo.join(CAMPAIGNS);
    let fragment = directory.join(format!("registry.d/{campaign}.json"));
    fs::create_dir_all(fragment.parent().unwrap()).unwrap();
    fs::write(
        fragment,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({
                "manifest": format!("{CAMPAIGNS}/{campaign}.json")
            }))
            .unwrap()
        ),
    )
    .unwrap();
    fs::write(directory.join(format!("{campaign}.json")), "{}\n").unwrap();
}

#[test]
fn split_moves_every_row_into_one_fragment_per_campaign() {
    let case = Case::new();
    let repo = case.root().join("repo");
    let registry = registry(
        &repo,
        json!([
            {"manifest": "conductor/mutation_campaigns/zz_last.json"},
            {"manifest": "conductor/mutation_campaigns/aa_first.json"}
        ]),
    );
    assert_eq!(
        loaded_manifests(&registry, &repo),
        [
            "conductor/mutation_campaigns/zz_last.json",
            "conductor/mutation_campaigns/aa_first.json"
        ]
    );

    assert_eq!(
        split(&repo).unwrap(),
        [
            "conductor/mutation_campaigns/registry.d/zz_last.json",
            "conductor/mutation_campaigns/registry.d/aa_first.json"
        ]
    );
    let fragment = repo.join("conductor/mutation_campaigns/registry.d/zz_last.json");
    let text = fs::read_to_string(fragment).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({"manifest": "conductor/mutation_campaigns/zz_last.json"})
    );
    assert!(text.ends_with('\n'));
    assert_eq!(
        loaded_manifests(&registry, &repo),
        [
            "conductor/mutation_campaigns/aa_first.json",
            "conductor/mutation_campaigns/zz_last.json"
        ]
    );
    let payload: Value = serde_json::from_slice(&fs::read(registry).unwrap()).unwrap();
    assert_eq!(payload["campaigns"], json!([]));
    assert_eq!(payload["enforcement"], "changed_tests");
    assert_eq!(payload["test_patterns"], json!(canonical_patterns()));
}

#[test]
fn split_is_idempotent() {
    let case = Case::new();
    let repo = case.root().join("repo");
    registry(
        &repo,
        json!([{"manifest": "conductor/mutation_campaigns/a.json"}]),
    );
    assert_eq!(
        split(&repo).unwrap(),
        ["conductor/mutation_campaigns/registry.d/a.json"]
    );
    let before = snapshot(&repo);
    assert!(split(&repo).unwrap().is_empty());
    assert_eq!(snapshot(&repo), before);
}

#[test]
fn split_refuses_before_writing_anything() {
    let case = Case::new();
    let repo = case.root().join("repo");
    let fragments = repo.join("conductor/mutation_campaigns/registry.d");
    let campaign_error = Python::attach(|py| {
        module(py, "conductor.mutation_scope")
            .getattr("CampaignError")
            .unwrap()
            .unbind()
    });
    registry(
        &repo,
        json!([
            {"manifest": "conductor/mutation_campaigns/dup.json"},
            {"manifest": "conductor/mutation_campaigns/dup.json"}
        ]),
    );
    Python::attach(|py| {
        assert_error(
            py,
            split(&repo).unwrap_err(),
            campaign_error.bind(py),
            "share the campaign id 'dup'",
        );
    });
    assert!(!fragments.exists());

    registry(
        &repo,
        json!([
            {"manifest": "conductor/mutation_campaigns/ok.json"},
            {"note": "no manifest"}
        ]),
    );
    Python::attach(|py| {
        assert_error(
            py,
            split(&repo).unwrap_err(),
            campaign_error.bind(py),
            "has no manifest string",
        );
    });
    assert!(!fragments.exists());

    let registry = registry(
        &repo,
        json!([{"manifest": "conductor/mutation_campaigns/a.json"}]),
    );
    fs::create_dir_all(&fragments).unwrap();
    let fragment = fragments.join("a.json");
    fs::write(
        &fragment,
        "{\"manifest\": \"conductor/mutation_campaigns/other.json\"}\n",
    )
    .unwrap();
    let before = fs::read(&fragment).unwrap();
    Python::attach(|py| {
        assert_error(
            py,
            split(&repo).unwrap_err(),
            campaign_error.bind(py),
            "already registers a different manifest",
        );
    });
    assert_eq!(fs::read(fragment).unwrap(), before);
    let payload: Value = serde_json::from_slice(&fs::read(registry).unwrap()).unwrap();
    assert_eq!(
        payload["campaigns"],
        json!([{"manifest": "conductor/mutation_campaigns/a.json"}])
    );
}

#[test]
fn two_branches_each_adding_a_campaign_merge_cleanly() {
    let case = Case::new();
    let repo = case.root().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--quiet", "-b", "main"]);
    git(&repo, &["config", "user.name", "Registry Split Test"]);
    git(&repo, &["config", "user.email", "split@test.invalid"]);
    let registry = registry(
        &repo,
        json!([
            {"manifest": "conductor/mutation_campaigns/one.json"},
            {"manifest": "conductor/mutation_campaigns/two.json"}
        ]),
    );
    split(&repo).unwrap();
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &["commit", "--quiet", "-m", "migrated registry layout"],
    );

    for (branch, campaign) in [("lane-a", "three"), ("lane-b", "four")] {
        git(&repo, &["checkout", "--quiet", "-b", branch]);
        register(&repo, campaign);
        git(&repo, &["add", "-A"]);
        git(
            &repo,
            &["commit", "--quiet", "-m", &format!("register {campaign}")],
        );
    }
    git(&repo, &["checkout", "--quiet", "main"]);
    git(&repo, &["merge", "--no-edit", "--quiet", "lane-a"]);
    git(&repo, &["merge", "--no-edit", "--quiet", "lane-b"]);
    assert_eq!(
        loaded_manifests(&registry, &repo),
        [
            "conductor/mutation_campaigns/four.json",
            "conductor/mutation_campaigns/one.json",
            "conductor/mutation_campaigns/three.json",
            "conductor/mutation_campaigns/two.json"
        ]
    );
}
