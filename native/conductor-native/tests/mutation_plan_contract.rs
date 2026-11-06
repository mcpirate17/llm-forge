#[cfg(feature = "source-analysis")]
use conductor_native::mutation_plan::PriorityOptions;
use conductor_native::mutation_plan::{compute_plan, PlanRequest};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
#[cfg(feature = "source-analysis")]
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "mutation_plan_contract/rust_ownership.rs"]
mod rust_ownership;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Tree(PathBuf);

impl Tree {
    #[cfg(feature = "source-analysis")]
    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[cfg(feature = "source-analysis")]
    fn priority(
        &self,
        scope: &[&str],
        budget: Option<f64>,
        telemetry: Option<&str>,
    ) -> Result<Value, String> {
        compute_plan(&PlanRequest {
            priority: Some(PriorityOptions {
                policy_path: "policy.toml".into(),
                base_ref: "HEAD".into(),
                telemetry_path: telemetry.map(str::to_owned),
                budget_seconds: budget,
            }),
            language: "rust".into(),
            repo_root: self.0.to_string_lossy().into_owned(),
            owner: "owner".into(),
            day: "20260910".into(),
            jobs: 2,
            python_jobs: None,
            fest_confined: false,
            mull_confined: false,
            run_timeout_seconds: 91,
            campaigns_root: "campaigns".into(),
            only_sources: Some(scope.iter().map(|p| (*p).into()).collect()),
            include_covered: false,
            extra_tests: BTreeMap::new(),
        })
    }
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
            priority: None,
            language: language.to_string(),
            repo_root: self.0.to_string_lossy().into_owned(),
            owner: "owner".to_string(),
            day: "20260910".to_string(),
            jobs: 2,
            python_jobs: None,
            fest_confined: false,
            mull_confined: false,
            run_timeout_seconds: 91,
            campaigns_root: "conductor/mutation_campaigns".to_string(),
            only_sources: scope.map(|paths| paths.iter().map(|path| (*path).to_string()).collect()),
            include_covered: false,
            extra_tests: BTreeMap::new(),
        })
    }
}

#[cfg(feature = "source-analysis")]
fn rust_fixture(tree: &Tree, name: &str, value: u64) {
    tree.write(
        "pyproject.toml",
        "[tool.conductor]\ncandidate_policy = 'policy.toml'\n",
    );
    tree.write(
        &format!("{name}/Cargo.toml"),
        &format!("[package]\nname = '{name}'\nversion = '0.1.0'\n"),
    );
    tree.write(&format!("{name}/src/lib.rs"), &format!("pub const VALUE: u64 = {value};\n#[cfg(test)] mod tests {{ #[test] fn checks() {{ assert!(VALUE > 0); }} }}\n"));
}

#[cfg(feature = "source-analysis")]
#[test]
fn priority_scope_policy_novelty_and_unknown_budget_are_explicit() {
    let tree = Tree::new();
    rust_fixture(&tree, "a", 1);
    rust_fixture(&tree, "b", 1);
    tree.write(
        "policy.toml",
        "[risk]\nhigh = ['a/*.rs','b/*.rs','c/*.rs']\n",
    );
    tree.git(&["init", "-q"]);
    tree.git(&["add", "."]);
    tree.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "base",
    ]);
    rust_fixture(&tree, "a", 2);
    rust_fixture(&tree, "c", 1);
    rust_fixture(&tree, "discovery", 1);
    let scope = [
        "a/src/lib.rs",
        "b/src/lib.rs",
        "c/src/lib.rs",
        "discovery/src/lib.rs",
    ];
    let result = tree.priority(&scope, None, None).unwrap();
    let rows = result["priority"]["ranked"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["sources"][0]["change"], "new");
    assert_eq!(rows[1]["sources"][0]["change"], "changed");
    assert!(result["priority"]["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["path"] == "discovery/src/lib.rs"));
    assert!(rows[1]["changed_tests"].as_array().unwrap().is_empty());
    assert!(rows[0]["mutation_evidence"]
        .as_str()
        .unwrap()
        .starts_with("unknown"));
    let budget = tree.priority(&scope, Some(10.0), None).unwrap();
    assert!(budget["manifests"].as_array().unwrap().is_empty());
    assert_eq!(
        budget["priority"]["ranked"][0]["decision"],
        "deferred-unknown-cost"
    );
    tree.write(
        "telemetry.json",
        &json!({"campaigns":{rows[0]["campaign_id"].as_str().unwrap():{
        "tree_oid":"stale","source_sha256":{},"test_sha256":{},"dependency_impact_hint":999,
        "cost_hint_seconds":0.1}}})
        .to_string(),
    );
    let stale = tree
        .priority(&scope, Some(10.0), Some("telemetry.json"))
        .unwrap();
    assert_eq!(
        stale["priority"]["ranked"][0]["telemetry_status"],
        "stale-or-incomplete"
    );
    tree.write("policy.toml", "[risk]\nhigh = []\n");
    assert!(tree.priority(&scope, None, None).unwrap()["manifests"]
        .as_array()
        .unwrap()
        .is_empty());
    fs::remove_file(tree.0.join("policy.toml")).unwrap();
    assert!(tree
        .priority(&scope, None, None)
        .unwrap_err()
        .contains("policy unavailable"));
}

#[cfg(feature = "source-analysis")]
#[test]
fn priority_changed_native_tests_report_missing_source_scope_without_expanding_it() {
    let tree = Tree::new();
    rust_fixture(&tree, "a", 1);
    tree.write("policy.toml", "[risk]\nhigh = ['a/*.rs']\n");
    tree.git(&["init", "-q"]);
    tree.git(&["add", "."]);
    tree.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "base",
    ]);
    tree.write(
        "a/tests/check.rs",
        "#[test] fn checks_new_behavior() { assert!(true); }\n",
    );
    let result = tree.priority(&["a/tests/check.rs"], None, None).unwrap();
    assert!(result["manifests"].as_array().unwrap().is_empty());
    assert_eq!(
        result["priority"]["changed_tests_without_campaign"][0]["path"],
        "a/tests/check.rs"
    );
}

#[cfg(feature = "source-analysis")]
#[test]
fn priority_ties_are_deterministic_and_invalid_scope_or_budget_is_refused() {
    let tree = Tree::new();
    tree.write("policy.toml", "[risk]\nhigh = ['*/*.rs']\n");
    tree.git(&["init", "-q"]);
    tree.git(&["add", "."]);
    tree.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "base",
    ]);
    rust_fixture(&tree, "z", 1);
    rust_fixture(&tree, "a", 1);
    let scope = ["z/src/lib.rs", "a/src/lib.rs"];
    let first = tree.priority(&scope, None, None).unwrap();
    let second = tree.priority(&[scope[1], scope[0]], None, None).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        first["priority"]["ranked"][0]["sources"][0]["path"],
        "a/src/lib.rs"
    );
    assert!(tree
        .priority(&[], None, None)
        .unwrap_err()
        .contains("nonempty"));
    assert!(tree
        .priority(&scope, Some(f64::NAN), None)
        .unwrap_err()
        .contains("finite"));
}

#[cfg(feature = "source-analysis")]
#[test]
fn priority_test_only_mixed_paths_and_nested_per_source_campaigns_preserve_engine_scope() {
    let tree = Tree::new();
    rust_fixture(&tree, "crate", 1);
    tree.write("crate/src/lib.rs", "pub mod foo; pub mod bar;\n");
    tree.write(
        "crate/src/foo.rs",
        "pub const VALUE: u64 = 1; #[cfg(test)] mod tests;\n",
    );
    tree.write("crate/src/bar.rs", "pub const VALUE: u64 = 1;\n");
    tree.write(
        "crate/src/foo/tests.rs",
        "#[test] fn checks() { assert!(true); }\n",
    );
    tree.write("policy.toml", "[risk]\nhigh=['crate/*.rs','pkg/*.py']\n");
    tree.write("pkg/subject.py", "VALUE = 1\n");
    tree.git(&["init", "-q"]);
    tree.git(&["add", "."]);
    tree.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "base",
    ]);
    tree.write(
        "crate/src/foo.rs",
        "pub const VALUE: u64 = 2; #[cfg(test)] mod tests;\n",
    );
    tree.write("crate/src/bar.rs", "pub const VALUE: u64 = 2;\n");
    tree.write("docs/readme.md", "new documentation\n");
    let scope = [
        "crate/src/foo.rs",
        "crate/src/bar.rs",
        "crate/Cargo.toml",
        "docs/readme.md",
    ];
    let result = tree.priority(&scope, None, None).unwrap();
    let manifests = result["manifests"].as_array().unwrap();
    assert_eq!(manifests.len(), 2);
    for manifest in manifests {
        assert_eq!(manifest["source_sha256"].as_object().unwrap().len(), 1);
        assert_eq!(manifest["generator"]["source"].as_array().unwrap().len(), 1);
        assert!(manifest["generator"]["source"][0]
            .as_str()
            .unwrap()
            .starts_with("src/"));
        assert!(manifest["test_sha256"]
            .get("crate/src/foo/tests.rs")
            .is_some());
    }
    assert_ne!(manifests[0]["campaign_id"], manifests[1]["campaign_id"]);
    tree.write("pkg/test_subject.py", "def test_subject(): assert True\n");
    let request: PlanRequest = serde_json::from_value(json!({"language":"python","repo_root":tree.0,
        "owner":"fixture","day":"20261031","campaigns_root":"campaigns","only_sources":["pkg/test_subject.py"],
        "priority":{"policy_path":"policy.toml","base_ref":"HEAD"}})).unwrap();
    let tests_only = compute_plan(&request).unwrap();
    assert!(tests_only["manifests"].as_array().unwrap().is_empty());
    assert_eq!(
        tests_only["priority"]["changed_tests_without_campaign"][0]["path"],
        "pkg/test_subject.py"
    );
}

#[cfg(feature = "source-analysis")]
#[test]
fn priority_policy_cannot_override_the_hosts_configured_discovery_boundary() {
    let tree = Tree::new();
    rust_fixture(&tree, "discovery", 1);
    tree.write("policy.toml", "[risk]\nhigh=[]\n");
    tree.write("permissive.toml", "[risk]\nhigh=['*']\n");
    tree.git(&["init", "-q"]);
    tree.git(&["add", "."]);
    tree.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "base",
    ]);
    rust_fixture(&tree, "discovery", 2);
    let request: PlanRequest = serde_json::from_value(json!({"language":"rust","repo_root":tree.0,
        "owner":"fixture","day":"20261031","campaigns_root":"campaigns","only_sources":["discovery/src/lib.rs"],
        "priority":{"policy_path":"permissive.toml","base_ref":"HEAD"}})).unwrap();
    assert!(compute_plan(&request)
        .unwrap_err()
        .contains("host configured candidate policy"));
    assert!(tree
        .priority(&["discovery/src/lib.rs"], None, None)
        .unwrap()["manifests"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[cfg(feature = "source-analysis")]
#[test]
fn priority_rename_has_no_novelty_and_supplied_cost_hints_are_bound_to_hashes() {
    let tree = Tree::new();
    rust_fixture(&tree, "a", 1);
    tree.write("a/src/original.rs","pub const VALUE: u64 = 1; #[cfg(test)] mod tests { #[test] fn checks() { assert!(true); } }\n");
    tree.write("a/src/lib.rs", "pub mod original;\n");
    tree.write("policy.toml", "[risk]\nhigh = ['a/*.rs']\n");
    tree.git(&["init", "-q"]);
    tree.git(&["add", "."]);
    tree.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "base",
    ]);
    tree.git(&["mv", "a/src/original.rs", "a/src/renamed.rs"]);
    tree.write("a/src/lib.rs", "pub mod renamed;\n");
    let scope = ["a/src/renamed.rs"];
    let result = tree.priority(&scope, None, None).unwrap();
    assert_eq!(
        result["priority"]["ranked"][0]["sources"][0]["change"],
        "unchanged"
    );
    let manifest = &result["manifests"][0];
    let id = manifest["campaign_id"].as_str().unwrap();
    tree.write("telemetry.json", &json!({"campaigns":{id:{
            "tree_oid":tree.git(&["rev-parse","HEAD^{tree}"]),"worktree_sha256":result["priority"]["worktree_sha256"],"policy_sha256":result["priority"]["policy_sha256"],"source_sha256":manifest["source_sha256"],
        "test_sha256":manifest["test_sha256"],"cost_hint_seconds":5.0,"dependency_impact_hint":3
    }}}).to_string());
    assert_eq!(
        tree.priority(&scope, Some(5.0), Some("telemetry.json"))
            .unwrap()["manifests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let small = tree
        .priority(&scope, Some(4.9), Some("telemetry.json"))
        .unwrap();
    assert_eq!(
        small["priority"]["ranked"][0]["decision"],
        "deferred-budget"
    );
}

#[cfg(feature = "source-analysis")]
#[test]
fn priority_excludes_verified_test_modules_and_defers_unreachable_source_roles() {
    let tree = Tree::new();
    rust_fixture(&tree, "crate", 1);
    tree.write("policy.toml", "[risk]\nhigh=['crate/*.rs']\n");
    tree.write(
        "crate/src/lib.rs",
        "pub const VALUE:u64=1; #[cfg(test)] mod checks;\n",
    );
    tree.write(
        "crate/src/checks.rs",
        "#[test] fn guards() { assert!(true); }\n",
    );
    tree.git(&["init", "-q"]);
    tree.git(&["add", "."]);
    tree.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "base",
    ]);
    tree.write(
        "crate/src/checks.rs",
        "#[test] fn guards() { assert_eq!(1,1); }\n",
    );
    tree.write(
        "crate/src/orphan.rs",
        "#[test] fn not_wired() { assert!(true); }\n",
    );
    let result = tree
        .priority(&["crate/src/checks.rs", "crate/src/orphan.rs"], None, None)
        .unwrap();
    assert!(result["manifests"].as_array().unwrap().is_empty());
    assert!(result["priority"]["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["path"] == "crate/src/checks.rs" && r["source_role"]["role"] == "test-only"));
    assert_eq!(
        result["priority"]["ranked"][0]["decision"],
        "deferred-unresolved-source-role"
    );
}

#[cfg(feature = "source-analysis")]
#[test]
fn priority_reports_changed_external_test_wiring_as_unknown_identity() {
    let tree = Tree::new();
    rust_fixture(&tree, "crate", 1);
    tree.write("policy.toml", "[risk]\nhigh=['crate/*.rs']\n");
    tree.write("crate/src/lib.rs", "pub const VALUE:u64=1;\n");
    tree.write(
        "crate/src/checks.rs",
        "#[test] fn guards() { assert!(true); }\n",
    );
    tree.git(&["init", "-q"]);
    tree.git(&["add", "."]);
    tree.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "base",
    ]);
    tree.write(
        "crate/src/lib.rs",
        "pub const VALUE:u64=1; #[cfg(test)] mod checks;\n",
    );
    let result = tree.priority(&["crate/src/lib.rs"], None, None).unwrap();
    let row = &result["priority"]["ranked"][0];
    assert!(row["changed_tests"].as_array().unwrap().is_empty());
    assert_eq!(row["test_identity_gaps"].as_array().unwrap().len(), 1);
}

#[cfg(feature = "source-analysis")]
#[test]
#[ignore = "controlled planner latency benchmark; run explicitly with --nocapture"]
fn priority_latency_benchmark() {
    use std::{hint::black_box, time::Instant};
    const MODULES: usize = 64;
    const CHANGED: usize = 8;
    let tree = Tree::new();
    rust_fixture(&tree, "crate", 1);
    tree.write("policy.toml", "[risk]\nhigh=['crate/*.rs']\n");
    let mut root =
        String::from("#[cfg(test)] mod tests { #[test] fn baseline() { assert!(true); } }\n");
    for index in 0..MODULES {
        root.push_str(&format!("pub mod module_{index};\n"));
        let source: String = (0..16)
            .map(|f| format!("pub fn function_{f}()->u64 {{ {index} }}\n"))
            .collect();
        tree.write(&format!("crate/src/module_{index}.rs"), &source);
    }
    tree.write("crate/src/lib.rs", &root);
    tree.git(&["init", "-q"]);
    tree.git(&["add", "."]);
    tree.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "base",
    ]);
    let scope: Vec<_> = (0..CHANGED)
        .map(|i| format!("crate/src/module_{i}.rs"))
        .collect();
    for path in &scope {
        let source = fs::read_to_string(tree.0.join(path)).unwrap();
        tree.write(path, &source.replace("->u64 {", "->u64 { 1 +"));
    }
    let paths: Vec<_> = scope.iter().map(String::as_str).collect();
    let mut normal = Vec::new();
    let mut priority = Vec::new();
    for iteration in 0..10 {
        for enabled in [false, true] {
            let started = Instant::now();
            let result = if enabled {
                tree.priority(&paths, None, None)
            } else {
                tree.plan("rust", Some(&paths))
            }
            .unwrap();
            let micros = started.elapsed().as_micros();
            assert_eq!(manifests(&result).len(), if enabled { CHANGED } else { 1 });
            black_box(result);
            if iteration >= 2 {
                if enabled {
                    priority.push(micros);
                } else {
                    normal.push(micros);
                }
            }
        }
    }
    normal.sort_unstable();
    priority.sort_unstable();
    println!(
        "{}",
        json!({"modules":MODULES,"functions_per_module":16,"changed_sources":CHANGED,"warmups":2,"samples":8,
        "ordinary_median_us":normal[4],"ordinary_p95_us":normal[7],"priority_median_us":priority[4],"priority_p95_us":priority[7],
        "profile":if cfg!(debug_assertions) { "debug" } else { "release" },"engines_executed":0})
    );
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
fn package_whose_targets_live_in_another_crate_is_not_a_subject() {
    let tree = Tree::new();
    tree.write(
        "cli/Cargo.toml",
        "[package]\nname = \"widget-cli\"\nversion = \"0.1.0\"\n\n\
         [[bin]]\nname = \"widget\"\npath = \"../crate/src/main.rs\"\n",
    );
    tree.write("crate/Cargo.toml", rust_crate());
    tree.write("crate/src/main.rs", "fn main() {}\n#[test]\nfn runs() {}\n");

    let plan = tree
        .plan("rust", None)
        .expect("the cli package owns no sources");

    let ids: Vec<&str> = plan["manifests"]
        .as_array()
        .expect("manifests")
        .iter()
        .map(|manifest| {
            manifest["generator"]["options"]["package"]
                .as_str()
                .expect("package")
        })
        .collect();
    assert_eq!(ids, ["widget-core"]);
}

#[test]
fn package_with_a_local_target_path_and_no_sources_is_still_refused() {
    let tree = Tree::new();
    tree.write(
        "cli/Cargo.toml",
        "[package]\nname = \"widget-cli\"\nversion = \"0.1.0\"\n\n\
         [[bin]]\nname = \"widget\"\npath = \"main.rs\"\n",
    );

    let error = tree.plan("rust", None).expect_err("missing target");
    assert!(
        error.contains("cannot resolve Rust target/module") && error.contains("main.rs"),
        "{error}"
    );
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
        priority: None,
        language: "python".to_string(),
        repo_root: tree.0.to_string_lossy().into_owned(),
        owner: "owner".to_string(),
        day: "20260910".to_string(),
        jobs: 2,
        python_jobs: None,
        fest_confined: false,
        mull_confined: false,
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
