use conductor_native::mutation_plan::{compute_plan, PlanRequest};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Tree(PathBuf);

impl Tree {
    fn new() -> Self {
        let serial = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "conductor-native-plan-contract-{}-{serial}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create temporary repository");
        Self(root)
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(path, contents).expect("write fixture");
    }

    fn plan(&self, language: &str, scope: Option<&[&str]>) -> Result<Value, String> {
        compute_plan(&PlanRequest {
            language: language.to_string(),
            repo_root: self.0.to_string_lossy().into_owned(),
            owner: "owner".to_string(),
            day: "20260910".to_string(),
            jobs: 2,
            python_jobs: None,
            run_timeout_seconds: 91,
            campaigns_root: "conductor/mutation_campaigns".to_string(),
            only_sources: scope.map(|paths| paths.iter().map(|path| (*path).to_string()).collect()),
            include_covered: false,
            extra_tests: BTreeMap::new(),
        })
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn manifests(plan: &Value) -> &[Value] {
    plan["manifests"].as_array().expect("manifests array")
}

fn rust_crate() -> &'static str {
    "[package]\nname = 'widget-core'\nversion = '0.1.0'\n[[bin]]\nname = 'wrong-bin'\n[dependencies]\nname = 'wrong-dependency'\n"
}

#[test]
fn cargo_package_identity_ignores_workspace_bin_and_dependency_names() {
    let tree = Tree::new();
    tree.write("Cargo.toml", "[workspace]\nmembers = ['crate']\n");
    tree.write("crate/Cargo.toml", rust_crate());
    tree.write("crate/src/lib.rs", "#[cfg(test)]\nmod unit {}\n");

    let result = tree.plan("rust", None).expect("plan package");
    assert_eq!(manifests(&result).len(), 1);
    let manifest = &manifests(&result)[0];
    assert_eq!(manifest["generator"]["options"]["package"], "widget-core");
    assert_eq!(
        manifest["generator"]["options"]["manifest_path"],
        "crate/Cargo.toml"
    );
    assert_eq!(
        manifest["test_argv"],
        json!([
            "cargo",
            "test",
            "--manifest-path",
            "crate/Cargo.toml",
            "--package",
            "widget-core"
        ])
    );
}

#[test]
fn package_with_no_rust_sources_is_refused() {
    let tree = Tree::new();
    tree.write("crate/Cargo.toml", rust_crate());

    let error = tree
        .plan("rust", None)
        .expect_err("empty package must fail");
    assert_eq!(error, "crate crate declares a package but has no src/*.rs");
}

#[test]
fn cargo_manifest_pins_inline_and_integration_tests_and_exact_source_scope() {
    let tree = Tree::new();
    tree.write("crate/Cargo.toml", rust_crate());
    tree.write("crate/src/lib.rs", "#[cfg(test)]\nmod unit {}\n");
    tree.write("crate/src/sibling.rs", "pub fn sibling() {}\n");
    tree.write("crate/tests/integration.rs", "#[test]\nfn works() {}\n");
    tree.write("crate/tests/.venv/generated.rs", "#[test]\nfn stale() {}\n");

    let result = tree
        .plan("rust", Some(&["crate/src/lib.rs"]))
        .expect("scoped plan");
    let manifest = &manifests(&result)[0];
    assert_eq!(manifest["campaign_id"], "owner_crate_cargo_20260910");
    assert_eq!(manifest["generator"]["source"], json!(["src/lib.rs"]));
    assert_eq!(manifest["generator"]["jobs"], 2);
    assert_eq!(manifest["generator"]["run_timeout_seconds"], 91);
    assert!(manifest["generator"]
        .get("mutant_timeout_seconds")
        .is_none());
    assert_eq!(manifest["generator"]["exclude"], json!([]));
    assert_eq!(manifest["environment"], json!({}));
    assert_eq!(manifest["survivor_baseline"], json!([]));
    assert_eq!(manifest["survivor_baseline_recorded"], false);
    let tests = manifest["test_sha256"].as_object().expect("test pins");
    assert_eq!(tests.len(), 2);
    assert!(tests.contains_key("crate/src/lib.rs"));
    assert!(tests.contains_key("crate/tests/integration.rs"));
    assert!(!tests.contains_key("crate/tests/.venv/generated.rs"));
    assert_eq!(manifest["source_sha256"].as_object().unwrap().len(), 1);
}

#[test]
fn python_manifest_keeps_engine_contract_and_rejects_skipped_trees() {
    let tree = Tree::new();
    tree.write("pkg/subject.py", "x = 1\n");
    tree.write("pkg/test_subject.py", "def test_subject(): pass\n");
    tree.write(
        ".venv/lib/python3.12/site-packages/rich/console.py",
        "x = 1\n",
    );
    tree.write(
        "venv-mut/lib/python3.12/site-packages/rich/test_console.py",
        "x = 1\n",
    );
    tree.write("target/debug/build.py", "x = 1\n");
    tree.write("node_modules/a/b.py", "x = 1\n");

    let result = tree.plan("python", None).expect("plan python");
    assert!(result["unpaired"].as_array().unwrap().is_empty());
    assert_eq!(manifests(&result).len(), 1);
    let manifest = &manifests(&result)[0];
    assert_eq!(manifest["campaign_id"], "owner_subject_fest_20260910");
    assert_eq!(manifest["generator"]["source"], json!(["pkg/subject.py"]));
    assert_eq!(
        manifest["generator"]["exclude"],
        json!(["**/test_*.py", "**/conftest.py"])
    );
    assert_eq!(manifest["generator"]["run_timeout_seconds"], 91);
    assert!(manifest["generator"].get("jobs").is_none());
    assert!(manifest["generator"]
        .get("mutant_timeout_seconds")
        .is_none());
    assert_eq!(
        manifest["test_argv"],
        json!([
            "python",
            "-m",
            "pytest",
            "-q",
            "--rootdir=.",
            "pkg/test_subject.py"
        ])
    );
    assert_eq!(manifest["test_sha256"].as_object().unwrap().len(), 1);
    assert_eq!(manifest["survivor_baseline_recorded"], false);
    assert!(manifest["survivor_baseline_note"]
        .as_str()
        .unwrap()
        .contains("FIRST engine run"));
}

#[test]
fn mirror_wins_over_unrelated_name_and_orphan_lines_are_reported() {
    let tree = Tree::new();
    tree.write("pkg/deep/subject.py", "x = 1\n");
    tree.write(
        "pkg/tests/deep/test_subject.py",
        "def test_subject(): pass\n",
    );
    tree.write("elsewhere/test_subject.py", "def test_other(): pass\n");
    tree.write("pkg/deep/orphan.py", "x = 1\nx = 2\n");

    let result = tree.plan("python", None).expect("plan mirror");
    assert_eq!(
        result["unpaired"],
        json!([{"source": "pkg/deep/orphan.py", "lines": 2}])
    );
    assert_eq!(result["unpaired_lines"], 2);
    let manifest = &manifests(&result)[0];
    assert_eq!(
        manifest["test_argv"].as_array().unwrap().last().unwrap(),
        "pkg/tests/deep/test_subject.py"
    );
}

#[test]
fn slug_collisions_disambiguate_without_changing_unique_campaign_ids() {
    let tree = Tree::new();
    for (source, test) in [
        ("conductor/gate.py", "conductor/test_gate.py"),
        ("research/tools/gate.py", "research/tests/test_gate.py"),
        ("conductor/unique.py", "conductor/test_unique.py"),
    ] {
        tree.write(source, "x = 1\n");
        tree.write(test, "def test_subject(): pass\n");
    }

    let result = tree.plan("python", None).expect("plan collided names");
    let ids: Vec<&str> = manifests(&result)
        .iter()
        .map(|manifest| manifest["campaign_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "owner_conductor_gate_fest_20260910",
            "owner_unique_fest_20260910",
            "owner_tools_gate_fest_20260910"
        ]
    );
}

#[test]
fn covered_subject_is_skipped_until_include_covered_requests_a_narrow_campaign() {
    let tree = Tree::new();
    tree.write("pkg/subject.py", "x = 1\n");
    tree.write("pkg/test_subject.py", "def test_subject(): pass\n");
    tree.write(
        "conductor/mutation_campaigns/wide.json",
        &json!({
            "mutation_engine": "fest",
            "generator": {"source": ["pkg/subject.py"]}
        })
        .to_string(),
    );

    let request = PlanRequest {
        language: "python".to_string(),
        repo_root: tree.0.to_string_lossy().into_owned(),
        owner: "owner".to_string(),
        day: "20260910".to_string(),
        jobs: 2,
        python_jobs: None,
        run_timeout_seconds: 91,
        campaigns_root: "conductor/mutation_campaigns".to_string(),
        only_sources: Some(vec!["pkg/subject.py".to_string()]),
        include_covered: false,
        extra_tests: BTreeMap::new(),
    };
    let skipped = compute_plan(&request).expect("covered source");
    assert!(manifests(&skipped).is_empty());
    assert_eq!(skipped["already_covered"], json!(["pkg/subject.py"]));

    let admitted = compute_plan(&PlanRequest {
        include_covered: true,
        ..request
    })
    .expect("explicit second campaign");
    assert_eq!(manifests(&admitted).len(), 1);
    assert_eq!(admitted["already_covered"], json!([]));
}

fn ambiguous_pair(tree: &Tree, dir: &str) {
    tree.write(&format!("{dir}/model.py"), "x = 1\n");
    tree.write("t_a/test_model.py", "def test_a(): pass\n");
    tree.write("t_b/test_model.py", "def test_b(): pass\n");
}

#[test]
fn scoped_plan_ignores_ambiguous_out_of_scope_source() {
    let tree = Tree::new();
    ambiguous_pair(&tree, "other");
    tree.write("pkg/widget.py", "y = 2\n");
    tree.write("pkg/test_widget.py", "def test_w(): pass\n");

    let result = tree
        .plan("python", Some(&["pkg/widget.py"]))
        .expect("scoped plan");
    let planned = manifests(&result);
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0]["generator"]["source"], json!(["pkg/widget.py"]));
    assert!(
        tree.plan("python", None).is_err(),
        "unscoped run still sees the ambiguity"
    );
}

#[test]
fn scoped_plan_still_refuses_ambiguous_in_scope_source() {
    let tree = Tree::new();
    ambiguous_pair(&tree, "other");
    let error = tree
        .plan("python", Some(&["other/model.py"]))
        .expect_err("in-scope ambiguity is fatal");
    assert!(error.contains("other/model.py"), "{error}");
    assert!(error.contains("test_model.py"), "{error}");
}

#[test]
fn in_scope_source_pairs_with_test_outside_scope_directory() {
    let tree = Tree::new();
    tree.write("src/pkg/widget.py", "y = 2\n");
    tree.write("tests/test_widget.py", "def test_w(): pass\n");

    let result = tree
        .plan("python", Some(&["src/pkg/widget.py"]))
        .expect("scoped plan");
    let planned = manifests(&result);
    assert_eq!(planned.len(), 1);
    assert_eq!(
        planned[0]["generator"]["source"],
        json!(["src/pkg/widget.py"])
    );
    assert!(planned[0].to_string().contains("tests/test_widget.py"));
}

#[test]
fn python_workers_are_explicit_and_do_not_reuse_rust_jobs() {
    let tree = Tree::new();
    tree.write("pkg/subject.py", "x = 1\n");
    tree.write("pkg/test_subject.py", "def test_subject(): pass\n");
    for width in [None, Some(1), Some(2)] {
        let request: PlanRequest = serde_json::from_value(json!({
            "language": "python", "repo_root": tree.0, "owner": "fixture", "day": "20261004",
            "jobs": 4, "python_jobs": width, "campaigns_root": "campaigns",
            "only_sources": ["pkg/subject.py"]
        }))
        .unwrap();
        let result = compute_plan(&request).unwrap();
        let generator = &manifests(&result)[0]["generator"];
        assert_eq!(generator.get("jobs").and_then(Value::as_i64), width);
    }
    tree.write("crate/Cargo.toml", rust_crate());
    tree.write("crate/src/lib.rs", "#[cfg(test)] mod tests {}\n");
    let request: PlanRequest = serde_json::from_value(json!({
        "language": "rust", "repo_root": tree.0, "owner": "fixture", "day": "20261004",
        "campaigns_root": "campaigns", "only_sources": ["crate/src/lib.rs"]
    }))
    .unwrap();
    assert_eq!(
        manifests(&compute_plan(&request).unwrap())[0]["generator"]["jobs"],
        4
    );
}

#[test]
fn python_worker_validation_fails_before_source_discovery() {
    let tree = Tree::new();
    for width in [0, -1] {
        let request: PlanRequest = serde_json::from_value(json!({
            "language": "python", "repo_root": tree.0, "owner": "fixture", "day": "20261004",
            "python_jobs": width, "campaigns_root": "campaigns"
        }))
        .unwrap();
        assert!(compute_plan(&request)
            .unwrap_err()
            .contains("worker count must be positive"));
    }
    let request: PlanRequest = serde_json::from_value(json!({
        "language": "rust", "repo_root": tree.0, "owner": "fixture", "day": "20261004",
        "python_jobs": 2, "campaigns_root": "campaigns"
    }))
    .unwrap();
    assert!(compute_plan(&request).unwrap_err().contains("Python-only"));
}
