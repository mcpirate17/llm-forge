#![cfg(feature = "python-compat-tests")]
//! Real Git benchmark fixture assertions, owned by Rust rather than pytest.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{module, path, text, AttrPatch, Case};

const POLICY: &str = r#"schema_version = 1
block_at = "high"
max_workers = 1
cache_ttl_days = 1
claim_max_age_hours = 1
max_file_bytes = 1000000
max_binary_bytes = 1000000
coverage_threshold = 75.0
high_risk_coverage_threshold = 90.0
baseline_expires = 2099-01-01
exceptions = []

[classes]

[risk]
high = []

[paths]
protected_deletes = []
hot = []
generated = []

[checks.candidate-integrity]
kind = "builtin"
profiles = ["fast", "full"]
classes = []
severity = "critical"
always = true
cache = false
run_on_deletions = true
timeout_seconds = 10
memory_mb = 128
max_output_chars = 1000
"#;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn source_content(relative: &str) -> Vec<u8> {
    let forge = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let actual = forge.join("src").join(relative);
    let text = match relative {
        "conductor/candidate_policy.toml" => POLICY,
        "conductor/vulture_baseline.json" => "{\"schema_version\":1,\"generated_from_tree\":[\"00000000\",\"00000000\",\"00000000\",\"00000000\",\"00000000\"],\"expires\":\"2099-01-01\",\"count\":0,\"entries\":{}}",
        ".github/CODEOWNERS" => "* @mcpirate17\n",
        ".github/workflows/governance-ci.yml" => "name: governance-ci\non: [push]\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        ".pre-commit-config.yaml" => "repos: []\n",
        "AGENTS.md" => "# Synthetic benchmark fixture\n",
        "Makefile" => ".PHONY: noop\nnoop:\n\t@true\n",
        "research/notes/unified_candidate_review_architecture_2026-08-16.md" => "# Synthetic benchmark fixture\n",
        _ => {
            if actual.is_file() { return fs::read(actual).unwrap(); }
            if relative == ".gitignore" && forge.join(".gitignore").is_file() {
                return fs::read(forge.join(".gitignore")).unwrap();
            }
            "# benchmark fixture path\n"
        }
    };
    text.as_bytes().to_vec()
}

fn source(case: &Case, py: Python<'_>, benchmark: &Bound<'_, PyAny>) -> PathBuf {
    let source = case.mkdir("source");
    git(&source, &["init", "-q", "-b", "main"]);
    git(&source, &["config", "user.name", "Candidate Review Test"]);
    git(
        &source,
        &["config", "user.email", "candidate-review@example.invalid"],
    );
    git(&source, &["config", "commit.gpgsign", "false"]);
    git(&source, &["config", "core.hooksPath", "/dev/null"]);
    for item in benchmark
        .getattr("GOVERNANCE_PATHS")
        .unwrap()
        .try_iter()
        .unwrap()
    {
        let relative = item.unwrap().extract::<String>().unwrap();
        let target = source.join(&relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, source_content(&relative)).unwrap();
    }
    let review_source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/conductor/candidate_review");
    let review_target = source.join("conductor/candidate_review");
    fs::create_dir_all(&review_target).unwrap();
    for entry in fs::read_dir(review_source).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().is_some_and(|ext| ext == "py") {
            fs::copy(entry.path(), review_target.join(entry.file_name())).unwrap();
        }
    }
    fs::write(
        source.join("package-lock.json"),
        "{\"lockfileVersion\": 1}\n",
    )
    .unwrap();
    let paths = benchmark
        .getattr("_source_paths")
        .unwrap()
        .call1((path(py, &source),))
        .unwrap();
    assert!(!paths.is_empty().unwrap());
    git(&source, &["add", "--all"]);
    git(
        &source,
        &[
            "commit",
            "-qm",
            "isolated benchmark source\n\nAgent: llm-fixture",
        ],
    );
    source
}

#[test]
fn benchmark_uses_isolated_real_git_candidates_and_preserves_cold_warm_identity() {
    let case = Case::new();
    Python::attach(|py| {
        let benchmark = module(py, "conductor.candidate_review.benchmark");
        let source = source(&case, py, benchmark.as_any());
        let fixture = benchmark
            .getattr("_prepare_fixture")
            .unwrap()
            .call1((path(py, &source), path(py, case.root())))
            .unwrap();
        let repo = fixture.getattr("repo").unwrap();
        let mut trees = std::collections::BTreeMap::new();
        for scenario in benchmark.getattr("SCENARIOS").unwrap().try_iter().unwrap() {
            let scenario = scenario.unwrap().extract::<String>().unwrap();
            let index = benchmark
                .getattr("_scenario_index")
                .unwrap()
                .call1((&fixture, path(py, case.root()), &scenario))
                .unwrap();
            let kwargs = PyDict::new(py);
            kwargs.set_item("index", index).unwrap();
            let tree = benchmark
                .getattr("_git")
                .unwrap()
                .call((&repo, vec!["write-tree"]), Some(&kwargs))
                .unwrap();
            trees.insert(scenario, text(&tree));
        }
        assert_eq!(
            trees
                .values()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            trees.len() - 1
        );
        assert_eq!(trees["small-python"], trees["full-review"]);
        let docs = benchmark
            .getattr("_scenario_index")
            .unwrap()
            .call1((&fixture, path(py, case.root()), "docs-only"))
            .unwrap();
        let cold = benchmark
            .getattr("_review_once")
            .unwrap()
            .call1((&fixture, path(py, case.root()), "docs-only", &docs, "cold"))
            .unwrap();
        let warm = benchmark
            .getattr("_review_once")
            .unwrap()
            .call1((&fixture, path(py, case.root()), "docs-only", &docs, "warm"))
            .unwrap();
        for row in [&cold, &warm] {
            assert_eq!(text(&row.get_item("decision").unwrap()), "pass");
            assert_eq!(text(&row.get_item("tree_oid").unwrap()), trees["docs-only"]);
        }
        assert_eq!(
            benchmark
                .getattr("_finding_counts")
                .unwrap()
                .call1(("invalid",))
                .unwrap()
                .len()
                .unwrap(),
            0
        );
        assert_eq!(
            benchmark
                .getattr("_finding_summary")
                .unwrap()
                .call1(("invalid",))
                .unwrap()
                .len()
                .unwrap(),
            0
        );
        let error = benchmark
            .getattr("_scenario_index")
            .unwrap()
            .call1((&fixture, path(py, case.root()), "unknown"))
            .unwrap_err();
        support::assert_error(
            py,
            error,
            &benchmark.getattr("BenchmarkError").unwrap(),
            "unknown",
        );
    });
}

#[test]
fn benchmark_cli_writes_the_selected_result_payload() {
    let case = Case::new();
    Python::attach(|py| {
        let benchmark = module(py, "conductor.candidate_review.benchmark");
        let payload = PyDict::new(py);
        payload.set_item("schema_version", 1).unwrap();
        payload.set_item("scenarios", PyDict::new(py)).unwrap();
        let mock = PyModule::import(py, "unittest.mock")
            .unwrap()
            .getattr("Mock")
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("return_value", &payload).unwrap();
        let fake = mock.call((), Some(&kwargs)).unwrap();
        let _patch = AttrPatch::replace(&benchmark, "run_benchmarks", &fake);
        let output = case.root().join("benchmark-main.json");
        let args = vec![
            "--repo".to_owned(),
            case.root().to_str().unwrap().to_owned(),
            "--output".to_owned(),
            output.to_str().unwrap().to_owned(),
            "--scenario".to_owned(),
            "docs-only".to_owned(),
        ];
        assert_eq!(
            benchmark
                .getattr("main")
                .unwrap()
                .call1((args,))
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            0
        );
        let actual: serde_json::Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
        assert_eq!(
            actual,
            serde_json::json!({"schema_version":1,"scenarios":{}})
        );
    });
}
