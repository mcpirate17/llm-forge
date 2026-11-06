#![cfg(target_os = "linux")]
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::Case;
use git2::{Repository, Signature};
use serde_json::{json, Value};
use sha2::Digest;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

fn command() -> Command {
    let mut command = Command::new(support::forge_binary::path());
    command
        .env_clear()
        .env("PATH", "")
        .args(["candidate", "benchmark"]);
    command
}
fn output(mut command: Command, expected: i32) -> Value {
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(90), 1024 * 1024).unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn fixture() -> Case {
    let mut case = Case::new(Path::new("/bin/true"));
    case.fixture.stage("package-lock.json", "{}\n");
    case.fixture.stage(
        "forge.toml",
        "schema_version=1\n[conductor]\ncandidate_policy='policies/main.toml'\n",
    );
    case.grants["batch"] =
        json!({"max_workers":1,"cpu_millis":1000,"memory_mib":256,"processes":16});
    let repo = Repository::open(case.fixture.root.join("repo")).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let signature = Signature::now("benchmark test", "test@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "captured benchmark inputs",
        &tree,
        &[&parent],
    )
    .unwrap();
    case
}
fn prepare(case: &Case, scenarios: &[&str]) -> (PathBuf, Value) {
    let root = case.fixture.root.join("benchmark-data");
    let mut cmd = command();
    cmd.arg("prepare")
        .arg("--repo")
        .arg(case.fixture.root.join("repo"))
        .arg("--fixture")
        .arg(&root);
    for scenario in scenarios {
        cmd.args(["--scenario", scenario]);
    }
    let value = output(cmd, 0);
    assert_eq!(
        fs::metadata(root.join(".git"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    (root, value["manifest"].clone())
}
fn run(root: &Path, report: &Path) -> Command {
    let mut cmd = command();
    cmd.arg("run")
        .arg("--fixture")
        .arg(root)
        .arg("--output")
        .arg(report);
    cmd
}
fn failure(mut command: Command) -> Output {
    let result =
        forge::process_probe::run(&mut command, Duration::from_secs(20), 1024 * 1024).unwrap();
    assert!(!result.status.success());
    result
}
#[test]
fn prepares_seven_real_git_scenarios_without_interpreter_and_preserves_source() {
    let case = fixture();
    let before = case.source_state();
    let (root, manifest) = prepare(
        &case,
        &[
            "docs-only",
            "small-python",
            "native-code",
            "dependency",
            "large-delete-rename",
            "full-review",
            "native-rust",
        ],
    );
    assert!(!root.join(".git/objects/info/alternates").exists());
    let repo = Repository::open(&root).unwrap();
    let scenarios = manifest["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 7);
    assert_eq!(scenarios[1]["tree_oid"], scenarios[5]["tree_oid"]);
    assert_eq!(scenarios[5]["profile"], "full");
    for n in [0, 1, 2, 3, 4, 6] {
        assert_eq!(scenarios[n]["profile"], "fast");
    }
    let baseline = repo
        .find_tree(git2::Oid::from_str(manifest["baseline_tree"].as_str().unwrap()).unwrap())
        .unwrap();
    let python_path = conductor_native::candidate_benchmark::python_path("conductor").unwrap();
    let before_python = repo
        .find_blob(baseline.get_path(Path::new(&python_path)).unwrap().id())
        .unwrap();
    assert_eq!(
        before_python.content(),
        conductor_native::candidate_benchmark::PYTHON_BASE
    );
    let changed = repo
        .find_tree(git2::Oid::from_str(scenarios[1]["tree_oid"].as_str().unwrap()).unwrap())
        .unwrap();
    let after = repo
        .find_blob(changed.get_path(Path::new(&python_path)).unwrap().id())
        .unwrap();
    assert_eq!(
        after.content(),
        conductor_native::candidate_benchmark::python_input("conductor")
            .unwrap()
            .1
    );
    assert_eq!(scenarios[1]["changes"].as_array().unwrap().len(), 1);
    let delta = &scenarios[1]["changes"][0];
    assert_ne!(delta["before"]["sha256"], delta["after"]["sha256"]);
    let large = repo
        .find_tree(git2::Oid::from_str(scenarios[4]["tree_oid"].as_str().unwrap()).unwrap())
        .unwrap();
    for n in 0..500 {
        assert!(large
            .get_path(Path::new(&format!("benchmark_fixture/obsolete_{n:04}.txt")))
            .is_err());
    }
    for n in 250..500 {
        assert!(large
            .get_path(Path::new(&format!("benchmark_fixture/renamed_{n:04}.txt")))
            .is_ok());
    }
    let native = repo
        .find_tree(git2::Oid::from_str(scenarios[6]["tree_oid"].as_str().unwrap()).unwrap())
        .unwrap();
    let rust = repo
        .find_blob(
            native
                .get_path(Path::new(
                    conductor_native::candidate_benchmark::RUST_INPUT_PATH,
                ))
                .unwrap()
                .id(),
        )
        .unwrap();
    assert_eq!(
        rust.content(),
        conductor_native::candidate_benchmark::RUST_CHANGED
    );
    case.assert_preserved(&before);
}
#[test]
fn preparation_refuses_unknown_duplicate_existing_destination_and_missing_dependency() {
    for names in [vec!["unknown"], vec!["docs-only", "docs-only"]] {
        let case = fixture();
        let dest = case.fixture.root.join("benchmark-data");
        let mut cmd = command();
        cmd.arg("prepare")
            .arg("--repo")
            .arg(case.fixture.root.join("repo"))
            .arg("--fixture")
            .arg(&dest);
        for name in names {
            cmd.args(["--scenario", name]);
        }
        failure(cmd);
        assert!(!dest.exists());
    }
    let case = fixture();
    let (root, _) = prepare(&case, &["docs-only"]);
    let mut cmd = command();
    cmd.arg("prepare")
        .arg("--repo")
        .arg(case.fixture.root.join("repo"))
        .arg("--fixture")
        .arg(root);
    failure(cmd);
    let case = fixture();
    let repo = Repository::open(case.fixture.root.join("repo")).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("package-lock.json")).unwrap();
    index.write().unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now("test", "test@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "missing dependency input",
        &tree,
        &[&parent],
    )
    .unwrap();
    let dest = case.fixture.root.join("benchmark-data");
    let mut cmd = command();
    cmd.arg("prepare")
        .arg("--repo")
        .arg(case.fixture.root.join("repo"))
        .arg("--fixture")
        .arg(&dest)
        .args(["--scenario", "dependency"]);
    failure(cmd);
    assert!(!dest.exists());
    assert!(fs::read_dir(&case.fixture.root).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".forge-candidate")));
}
#[test]
fn dry_run_keeps_cold_warm_unexecuted_and_requires_explicit_grants_for_execution() {
    let case = fixture();
    let (root, _) = prepare(&case, &["docs-only"]);
    let path = case.fixture.root.join("report.json");
    let report = output(run(&root, &path), 0);
    assert_eq!(report["executed"], false);
    assert_eq!(report["status"], "planned");
    assert_eq!(report["scenarios"], json!({}));
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
        report
    );
    let mut cmd = run(&root, &path);
    cmd.args(["--execute", "--owner", "benchmark-test"]);
    let error = failure(cmd);
    assert!(String::from_utf8_lossy(&error.stderr).contains("grants"));
    assert!(!root.join(".git/governance/ownership-claims.json").exists());
}
#[test]
fn changed_index_or_manifest_is_not_another_scenario_or_cached_run() {
    let case = fixture();
    let (root, _) = prepare(&case, &["docs-only"]);
    let index = root.join(".git/forge-benchmark-docs-only.index");
    fs::write(index, b"invalid index").unwrap();
    let error = failure(run(&root, &case.fixture.root.join("report.json")));
    assert!(String::from_utf8_lossy(&error.stderr).contains("index changed"));
    let case = fixture();
    let (root, mut manifest) = prepare(&case, &["small-python"]);
    manifest["scenarios"][0]["profile"] = json!("full");
    fs::write(
        root.join(".git/forge-benchmark.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let error = failure(run(&root, &case.fixture.root.join("report.json")));
    assert!(String::from_utf8_lossy(&error.stderr).contains("profile changed"));
}
#[test]
fn original_repository_grants_are_not_rewritten_for_the_fixture() {
    let case = fixture();
    let (root, _) = prepare(&case, &["docs-only"]);
    let grant = case.fixture.root.join("benchmark-grants.json");
    let bytes = serde_json::to_vec(&case.grants).unwrap();
    fs::write(&grant, &bytes).unwrap();
    fs::set_permissions(&grant, fs::Permissions::from_mode(0o600)).unwrap();
    let mut cmd = run(&root, &case.fixture.root.join("report.json"));
    cmd.args(["--execute", "--owner", "benchmark-test", "--grants"])
        .arg(&grant);
    let error = failure(cmd);
    assert!(String::from_utf8_lossy(&error.stderr).contains("source_root"));
    assert_eq!(fs::read(grant).unwrap(), bytes);
}
#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT and current source-matching FORGE_TEST_BIN"]
fn actual_native_reviews_have_fresh_bound_receipts_for_both_runs() {
    let mut case = fixture();
    case.delegated();
    capture_engine(&case);
    let (root, manifest) = prepare(&case, &["docs-only"]);
    // Test operator explicitly authors grants for this exact fixture; production never rewrites them.
    case.grants["source_root"] = json!(root.canonicalize().unwrap());
    case.grants["read_only"]
        .as_array_mut()
        .unwrap()
        .push(json!(root.canonicalize().unwrap()));
    let grant = case.fixture.root.join("benchmark-grants.json");
    fs::write(&grant, serde_json::to_vec(&case.grants).unwrap()).unwrap();
    fs::set_permissions(&grant, fs::Permissions::from_mode(0o600)).unwrap();
    let before = case.source_state();
    let report_path = case.fixture.root.join("benchmark-report.json");
    let mut cmd = run(&root, &report_path);
    cmd.args([
        "--execute",
        "--owner",
        "benchmark-test",
        "--policy",
        "policies/main.toml",
        "--today",
        "2030-01-10",
        "--grants",
    ])
    .arg(&grant);
    cmd.arg("--fixture-sha256").arg(format!(
        "{:x}",
        sha2::Sha256::digest(fs::read(root.join(".git/forge-benchmark.json")).unwrap())
    ));
    let report = output(cmd, 0);
    assert_eq!(report["status"], "complete");
    let rows = report["scenarios"]["docs-only"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["run"], "cold");
    assert_eq!(rows[1]["run"], "warm");
    assert_ne!(rows[0]["receipt_path"], rows[1]["receipt_path"]);
    for row in rows {
        assert_eq!(row["tree_oid"], manifest["scenarios"][0]["tree_oid"]);
        assert!(row["max_rss_kib"].as_i64().unwrap() > 0);
        let receipt = forge::candidate_receipt::parse_and_verify(
            &fs::read(row["receipt_path"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt.payload()["executed"], true);
        assert_eq!(receipt.payload()["approval"], false);
        assert_eq!(
            receipt.payload()["cache"]["mode"],
            "disabled_fresh_execution"
        );
        assert_eq!(receipt.payload()["candidate"]["tree_oid"], row["tree_oid"]);
        let stages = receipt.payload()["stages"][0]["stages"].as_array().unwrap();
        assert!(!stages.is_empty());
        assert!(stages
            .iter()
            .all(|s| s["report"]["cleanup_complete"] == true
                && s["report"]["resources"]["verified"] == true));
    }
    case.assert_preserved(&before);
}
fn capture_engine(case: &Case) {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for path in forge::source_fingerprint::files(&source).unwrap() {
        let target = case.fixture.root.join("repo").join(&path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, fs::read(source.join(&path)).unwrap()).unwrap();
        case.fixture.add(path.to_str().unwrap());
    }
    case.staged_policy(|text| text.replace("[checks.zeta]", "[checks.zeta]\nalways=true"));
    let repo = Repository::open(case.fixture.root.join("repo")).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let signature = Signature::now("benchmark test", "test@example.invalid").unwrap();
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "captured native engine",
        &tree,
        &[&parent],
    )
    .unwrap();
}

#[test]
fn captured_src_layout_selects_frozen_python_data_without_depending_on_a_live_module() {
    let case = fixture();
    case.fixture.stage("forge.toml", "schema_version=1\n[conductor]\npackage_root='src/conductor'\ncandidate_policy='policies/main.toml'\n");
    let repo = Repository::open(case.fixture.root.join("repo")).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let signature = Signature::now("test", "test@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "src layout",
        &tree,
        &[&parent],
    )
    .unwrap();
    let (_, manifest) = prepare(&case, &["small-python", "full-review"]);
    assert_eq!(manifest["package_root"], "src/conductor");
    assert_eq!(
        manifest["scenarios"][0]["changes"][0]["after"]["path"],
        "src/conductor/candidate_review/benchmark_probe.py"
    );
    assert_eq!(
        manifest["scenarios"][0]["tree_oid"],
        manifest["scenarios"][1]["tree_oid"]
    );
    assert!(!case
        .fixture
        .root
        .join("repo/src/conductor/candidate_review/benchmark.py")
        .exists());
}

#[test]
fn real_native_coordinator_receipts_are_measured_without_requiring_guest_execution() {
    let mut case = fixture();
    let before = case.source_state();
    let (root, _) = prepare(&case, &["docs-only"]);
    case.grants["source_root"] = json!(root.canonicalize().unwrap());
    case.grants["read_only"]
        .as_array_mut()
        .unwrap()
        .push(json!(root.canonicalize().unwrap()));
    let grants = case.fixture.root.join("benchmark-grants.json");
    fs::write(&grants, serde_json::to_vec(&case.grants).unwrap()).unwrap();
    fs::set_permissions(&grants, fs::Permissions::from_mode(0o600)).unwrap();
    let sha = format!(
        "{:x}",
        sha2::Sha256::digest(fs::read(root.join(".git/forge-benchmark.json")).unwrap())
    );
    let mut cmd = run(&root, &case.fixture.root.join("benchmark.json"));
    cmd.args(["--execute", "--owner", "benchmark-test", "--grants"])
        .arg(&grants)
        .args([
            "--fixture-sha256",
            &sha,
            "--policy",
            "policies/main.toml",
            "--today",
            "2030-01-10",
        ]);
    let report = output(cmd, 0);
    assert_eq!(report["status"], "complete");
    let rows = report["scenarios"]["docs-only"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row["run"], ["cold", "warm"][index]);
        let raw = fs::read(row["receipt_path"].as_str().unwrap()).unwrap();
        assert_eq!(
            format!("{:x}", sha2::Sha256::digest(&raw)),
            row["receipt_sha256"]
        );
        let receipt = forge::candidate_receipt::parse_and_verify(&raw).unwrap();
        assert_eq!(receipt.payload()["executed"], true);
        assert_eq!(receipt.payload()["approval"], false);
        assert_eq!(receipt.payload()["candidate"]["tree_oid"], row["tree_oid"]);
        assert_eq!(
            receipt.payload()["timings"]["duration_ms"],
            row["engine_duration_ms"]
        );
        assert!(receipt.payload()["required_checks"]
            .as_array()
            .unwrap()
            .contains(&json!("engine-integrity")));
        assert_eq!(
            receipt.payload()["checks"][0]["check_id"],
            "engine-integrity"
        );
        if let Some(sha) = receipt.payload()["engine"]["executable_sha256"].as_str() {
            assert_eq!(sha.len(), 64);
        } else {
            assert_eq!(receipt.payload()["checks"][0]["status"], "error");
            assert_eq!(receipt.payload()["review_complete"], false);
        }
    }
    let claims = forge::ownership::load_claims(&root).unwrap().0;
    assert!(
        claims.is_empty(),
        "benchmark must release only the claims it created"
    );
    case.assert_preserved(&before);
}

#[test]
fn explicit_index_preparation_captures_staged_bytes_and_preserves_original_index() {
    let case = fixture();
    case.fixture
        .stage("baseline.txt", "prospective staged source bytes\n");
    let before = case.source_state();
    let root = case.fixture.root.join("staged-benchmark-data");
    let mut cmd = command();
    cmd.arg("prepare")
        .arg("--repo")
        .arg(case.fixture.root.join("repo"))
        .arg("--fixture")
        .arg(&root)
        .args(["--kind", "index", "--scenario", "docs-only"]);
    let prepared = output(cmd, 0);
    assert_eq!(prepared["manifest"]["source_kind"], "index");
    let repo = Repository::open(&root).unwrap();
    let tree = repo
        .find_tree(
            git2::Oid::from_str(prepared["manifest"]["baseline_tree"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
    let blob = repo
        .find_blob(tree.get_path(Path::new("baseline.txt")).unwrap().id())
        .unwrap();
    assert_eq!(blob.content(), b"prospective staged source bytes\n");
    case.assert_preserved(&before);
}

fn commit_collision(case: &Case) {
    let repo = Repository::open(case.fixture.root.join("repo")).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let author = Signature::now("collision test", "test@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &author,
        &author,
        "captured collision must survive",
        &tree,
        &[&parent],
    )
    .unwrap();
}
fn refuses_collision(path: &str, original: &str, scenario: &str) {
    let case = fixture();
    case.fixture
        .stage(original, "original captured bytes must remain unchanged\n");
    commit_collision(&case);
    let before = case.source_state();
    let expected_path = if path.starts_with(&format!("{original}/")) {
        original
    } else {
        path
    };
    let source = Repository::open(case.fixture.root.join("repo")).unwrap();
    let captured_tree = source
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .tree()
        .unwrap();
    let captured_oid = captured_tree.get_path(Path::new(original)).unwrap().id();
    let captured_bytes = source.find_blob(captured_oid).unwrap().content().to_vec();
    let original = case.fixture.root.join("repo").join(original);
    let bytes = fs::read(&original).unwrap();
    let destination = case.fixture.root.join("collision-data");
    let mut cmd = command();
    cmd.arg("prepare")
        .arg("--repo")
        .arg(case.fixture.root.join("repo"))
        .arg("--fixture")
        .arg(&destination)
        .args(["--scenario", scenario]);
    let failed = failure(cmd);
    let error = String::from_utf8_lossy(&failed.stderr);
    assert!(
        error.contains("collides") && error.contains(expected_path),
        "{error}"
    );
    assert!(!destination.exists());
    assert_eq!(fs::read(original).unwrap(), bytes);
    let reopened = Repository::open(case.fixture.root.join("repo")).unwrap();
    assert_eq!(
        reopened.find_blob(captured_oid).unwrap().content(),
        captured_bytes
    );
    case.assert_preserved(&before);
    assert!(fs::read_dir(&case.fixture.root).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".forge-candidate")));
}
#[test]
fn every_generated_path_family_refuses_leaf_directory_and_ancestor_collisions() {
    for (path, scenario) in [
        ("benchmark_fixture/obsolete_0000.txt", "docs-only"),
        ("benchmark_fixture/obsolete_0499.txt", "docs-only"),
        ("benchmark_fixture/renamed_0250.txt", "large-delete-rename"),
        ("benchmark_fixture/renamed_0499.txt", "large-delete-rename"),
        (
            "research/notes/candidate_review_benchmark_probe.md",
            "docs-only",
        ),
        (
            "conductor/candidate_review/test_benchmark_probe.c",
            "native-code",
        ),
        (
            "conductor/candidate_review/benchmark_probe.py",
            "small-python",
        ),
        ("native/benchmark_probe/src/lib.rs", "native-rust"),
        ("native/benchmark_probe/Cargo.toml", "native-rust"),
    ] {
        refuses_collision(path, path, scenario);
        refuses_collision(path, &format!("{path}/retained.txt"), scenario);
        refuses_collision(path, path.rsplit_once('/').unwrap().0, scenario);
    }
}
#[test]
fn unselected_probe_paths_remain_captured_source_and_are_not_replaced() {
    let case = fixture();
    let paths = [
        "research/notes/candidate_review_benchmark_probe.md",
        "conductor/candidate_review/test_benchmark_probe.c",
        "benchmark_fixture/renamed_0250.txt",
    ];
    for path in paths {
        case.fixture.stage(path, "retained unselected source\n");
    }
    commit_collision(&case);
    let before = case.source_state();
    let (root, manifest) = prepare(&case, &["small-python"]);
    let repo = Repository::open(root).unwrap();
    for oid in [
        &manifest["baseline_tree"],
        &manifest["scenarios"][0]["tree_oid"],
    ] {
        let tree = repo
            .find_tree(git2::Oid::from_str(oid.as_str().unwrap()).unwrap())
            .unwrap();
        for path in paths {
            let blob = repo
                .find_blob(tree.get_path(Path::new(path)).unwrap().id())
                .unwrap();
            assert_eq!(blob.content(), b"retained unselected source\n");
        }
    }
    case.assert_preserved(&before);
}

#[test]
fn configured_generated_paths_cannot_replace_each_other() {
    let case = fixture();
    case.fixture.stage("forge.toml", "schema_version=1\n[conductor]\npackage_root='native/benchmark_probe/src/lib.rs'\ncandidate_policy='policies/main.toml'\n");
    commit_collision(&case);
    let before = case.source_state();
    let destination = case.fixture.root.join("overlapping-inputs");
    let mut cmd = command();
    cmd.arg("prepare")
        .arg("--repo")
        .arg(case.fixture.root.join("repo"))
        .arg("--fixture")
        .arg(&destination)
        .args(["--scenario", "docs-only"]);
    let error = failure(cmd);
    assert!(String::from_utf8_lossy(&error.stderr).contains("generated benchmark paths conflict"));
    assert!(!destination.exists());
    case.assert_preserved(&before);
}
