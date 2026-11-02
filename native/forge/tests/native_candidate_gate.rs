#![cfg(target_os = "linux")]
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::Case;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Output,
    time::Duration,
};
fn hash(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}
fn write(path: &Path, data: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
}
fn write_private(path: &Path, data: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .unwrap();
    file.write_all(data).unwrap();
    assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
}
fn diagnostics(output: &Output) -> String {
    format!(
        "status={}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
fn pytest_phase<'a>(gate: &'a Value, output: &Output) -> &'a Value {
    gate["phases"]
        .as_array()
        .and_then(|phases| phases.iter().find(|row| row["name"] == "pytest-config"))
        .unwrap_or_else(|| {
            panic!(
                "missing pytest-config phase; report={gate:#}\n{}",
                diagnostics(output)
            )
        })
}
fn operator_bundle(case: &Case) -> (PathBuf, String) {
    fs::set_permissions(&case.fixture.root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(
        case.fixture.root.join("scratch"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let root = case.fixture.root.join("installed");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let components = [
        "conductor/mutation_campaign_model.py",
        "conductor/mutation_patch_apply.py",
        "conductor/mutation_receipt_build.py",
        "conductor/mutation_scope.py",
        "conductor/mutation_testing.py",
        "conductor/mutation_testing_support.py",
        "conductor/mutation_value.py",
        "conductor/snapshot_worktree.py",
        "tooling/native/conductor-native/src/mutation_evidence.rs",
        "tooling/native/conductor-native/src/mutation_manifest.rs",
        "tooling/native/conductor-native/src/mutation_receipt.rs",
    ];
    let bindings = [
        "conductor/mutation_engine_generated.py",
        "conductor/mutation_run_scope.py",
        "conductor/mutation_engine_cargo.py",
        "conductor/mutation_engine_fest.py",
        "conductor/mutation_engine_mull.py",
    ];
    let mut c = serde_json::Map::new();
    let mut b = serde_json::Map::new();
    for (index, name) in components.iter().chain(bindings.iter()).enumerate() {
        let path = root.join(format!("pin-{index}"));
        write_private(&path, name.as_bytes());
        let row = json!({"path":path,"sha256":hash(name.as_bytes())});
        if index < components.len() {
            c.insert((*name).into(), row);
        } else {
            b.insert((*name).into(), row);
        }
    }
    let bytes=serde_json::to_vec(&json!({"schema_version":1,"symbol_hash_protocol":"cpython-ast-dump-3.12",
        "package_root":root,"components":c,"bindings":b,"lineage":null,"external_invariant_probe":null})).unwrap();
    let path = case.fixture.root.join("runner-bundle.json");
    write_private(&path, &bytes);
    (path, hash(&bytes))
}
fn git_profile(case: &Case) -> (PathBuf, String, PathBuf, String) {
    let git = Path::new("/usr/bin/git").canonicalize().unwrap();
    let digest = hash(&fs::read(&git).unwrap());
    let system = std::env::var("FORGE_TEST_GIT_SYSTEM_ATTRIBUTES")
        .expect("explicit observed Git system attributes path");
    let attributes = if system == "none" {
        json!({"mode":"none"})
    } else {
        json!({"mode":"path","path":system})
    };
    let bytes = serde_json::to_vec(
        &json!({"schema_version":1,"git_sha256":digest,"system_attributes":attributes}),
    )
    .unwrap();
    let path = case.fixture.root.join("git-profile.json");
    write_private(&path, &bytes);
    (git, digest, path, hash(&bytes))
}
fn stage_corpus(case: &Case) {
    // The exported candidate must declare its corpus; a conventional filename
    // alone is not registry authority. Preserve the shared fixture's policy.
    case.fixture.stage("forge.toml", "schema_version=1\n[conductor]\ncandidate_policy='policies/main.toml'\nmutation_registry='corpus/registry.json'\n");
    let source = "def subject():\n    return 1\n";
    let test =
        "def test_subject():\n    from pkg.subject import subject\n    assert subject() == 1\n";
    case.fixture.stage("pkg/subject.py", source);
    case.fixture.stage("pkg/test_subject.py", test);
    let manifest = json!({"schema_version":1,"campaign_id":"subject","title":"Fixture","language":"python","mutation_engine":"fest",
        "generator":{"engine":"fest","source":["pkg/subject.py"],"run_timeout_seconds":900,"jobs":1},
        "source_sha256":{"pkg/subject.py":hash(source.as_bytes())},"test_sha256":{"pkg/test_subject.py":hash(test.as_bytes())},
        "test_argv":["python","-m","pytest","pkg/test_subject.py"],"survivor_baseline":[]});
    case.fixture.stage(
        "campaigns/campaign.json",
        &serde_json::to_string(&manifest).unwrap(),
    );
    let registry = json!({"schema_version":1,"enforcement":"changed_tests","test_patterns":conductor_native::mutation_evidence::canonical_test_patterns(),
        "receipt_directories":["campaigns/receipts"],"campaigns":[{"manifest":"campaigns/campaign.json"}]});
    case.fixture.stage(
        "corpus/registry.json",
        &serde_json::to_string(&registry).unwrap(),
    );
}
fn commit(case: &Case) -> String {
    let root = case.fixture.root.join("repo");
    let repo = git2::Repository::open(root).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let base = parent.id().to_string();
    let tree = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree).unwrap();
    let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "target",
        &tree,
        &[&parent],
    )
    .unwrap();
    base
}
fn command(case: &Case, base: &str, receipt: &Path, skip: bool) -> std::process::Command {
    let grants = case.fixture.root.join("grants.json");
    write_private(&grants, &serde_json::to_vec(&case.grants).unwrap());
    let (git, git_sha, profile, profile_sha) = git_profile(case);
    let (runner, runner_sha) = operator_bundle(case);
    let mut command = std::process::Command::new(support::forge_binary::path());
    command
        .args(["candidate", "gate", "--repo"])
        .arg(case.fixture.root.join("repo"))
        .args(["--kind", "range", "--surface", "ci", "--base"])
        .arg(base)
        .args([
            "--policy",
            "policies/main.toml",
            "--profile",
            "fast",
            "--grants",
        ])
        .arg(grants)
        .arg("--git")
        .arg(git)
        .arg("--git-sha256")
        .arg(git_sha)
        .arg("--git-build-profile")
        .arg(profile)
        .arg("--git-build-profile-sha256")
        .arg(profile_sha)
        .arg("--mutation-runner-bundle")
        .arg(runner)
        .arg("--mutation-runner-sha256")
        .arg(runner_sha)
        .arg("--json-out")
        .arg(receipt)
        .arg("--json")
        .env("HOME", &case.fixture.root)
        .env("XDG_STATE_HOME", case.fixture.root.join("state"))
        .env_remove("TMPDIR")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("CONDUCTOR_POLICY")
        .env_remove("CONDUCTOR_CANDIDATE_POLICY");
    if skip {
        command.arg("--skip-review");
    }
    command
}
#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT, source-matching FORGE_TEST_BIN, and established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn actual_ci_review_runs_despite_failed_corpus_and_is_bound_to_exported_tree() {
    let mut case = Case::new(Path::new("/bin/true"));
    case.delegated();
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":512,"processes":32});
    stage_corpus(&case);
    let (registry, registry_sha256) = stage_review_evidence(&case);
    let base = commit(&case);
    let before = case.source_state();
    let dirty = case.fixture.root.join("repo/corpus/registry.json");
    fs::write(&dirty, "{broken live registry").unwrap();
    let dirty_policy = case.fixture.root.join("repo/policies/main.toml");
    fs::write(&dirty_policy, "invalid = [").unwrap();
    let receipt = case.fixture.root.join("gate-review.json");
    let mut command = command(&case, &base, &receipt, false);
    command
        .arg("--registry")
        .arg(registry)
        .arg("--registry-sha256")
        .arg(registry_sha256);
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(120), 1024 * 1024).unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}\nreview receipt: {}",
        diagnostics(&output),
        fs::read_to_string(&receipt).unwrap_or_else(|error| format!("unavailable: {error}"))
    );
    let gate: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(gate["review_skipped"], false);
    let phases = gate["phases"].as_array().unwrap();
    assert!(phases
        .iter()
        .any(|phase| phase["name"] == "mutation-corpus" && phase["ok"] == false));
    assert!(phases
        .iter()
        .any(|phase| phase["name"] == "cost-budget-audit"
            && phase["evidence"]["status"] == "NO_DATA"));
    assert!(gate["phases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|phase| phase["name"] == "review"));
    let review: Value = serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    assert_eq!(review["executed"], true);
    assert_eq!(review["review_complete"], true);
    assert_eq!(review["execution_verified"], true);
    assert_eq!(review["incomplete"], json!([]));
    assert_eq!(review["checks"][0]["status"], "passed");
    assert_eq!(review["execution_scope"], "complete_active_profile");
    assert_eq!(review["binding"]["surface"], "ci");
    assert_eq!(fs::read_to_string(dirty).unwrap(), "{broken live registry");
    assert_eq!(fs::read_to_string(dirty_policy).unwrap(), "invalid = [");
    case.assert_preserved(&before);
}

fn stage_review_evidence(case: &Case) -> (PathBuf, String) {
    let build_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repository = git2::Repository::open(&build_root).unwrap();
    let head = repository.head().unwrap().target().unwrap();
    case.fixture.stage(
        "uv.lock",
        &format!("version = 1\n[[package]]\nname = 'conductor-tooling'\nsource = {{ git = 'https://example.invalid/forge?rev=main#{head}' }}\n"),
    );
    case.fixture.stage(
        "Cargo.toml",
        "[package]\nname='gate-review-fixture'\nversion='0.0.0'\nedition='2021'\n",
    );
    case.fixture.stage(
        "src/lib.rs",
        "#[path=\"security/test_unit.rs\"]\nmod test_unit;\n",
    );
    case.fixture.stage("src/security/test_unit.rs", "fn selected(x: u32) -> u32 { x + 1 }\n#[test]\nfn increment_is_monotone() { for x in 0..64 { assert!(selected(x) > x); } }\n");
    // Use the real operator-supplied registry, consolidated as the native
    // contract requires, rather than inventing an empty mapping for JSON inputs.
    let root = build_root.join("native/conductor-native/src");
    let mut registry = fs::read_to_string(root.join("python_contract_targets.tsv")).unwrap();
    let include = "# include: python_contract_targets_extra.tsv";
    if registry.lines().any(|line| line == include) {
        registry = registry.replace(include, "");
        registry.push('\n');
        registry
            .push_str(&fs::read_to_string(root.join("python_contract_targets_extra.tsv")).unwrap());
    }
    let path = case.fixture.root.join("operator-contracts.tsv");
    write_private(&path, registry.as_bytes());
    (path, hash(registry.as_bytes()))
}

#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT, source-matching FORGE_TEST_BIN, and established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn missing_declared_tool_refuses_before_review_or_receipt() {
    let mut case = Case::new(Path::new("/bin/true"));
    case.delegated();
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":512,"processes":32});
    case.staged_policy(|policy| format!("{policy}\n[tools.unavailable]\nexecutable='not-granted'\nversion_command=['not-granted','--version']\nexpected_version='1'\nrequired_profiles=['fast']\nprovided_by='fixture'\nrationale='fixture'\n"));
    write(
        &case.fixture.root.join("repo/node_modules/.bin/not-granted"),
        b"#!/bin/sh\nexit 0\n",
    );
    stage_corpus(&case);
    let base = commit(&case);
    let before = case.source_state();
    let receipt = case.fixture.root.join("must-not-exist.json");
    let mut command = command(&case, &base, &receipt, false);
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(60), 1024 * 1024).unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let gate: Value = serde_json::from_slice(&output.stdout).unwrap();
    let phase = gate["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "tool-preflight")
        .unwrap();
    assert_eq!(phase["ok"], false);
    assert_eq!(phase["evidence"]["missing"], json!(["unavailable"]));
    assert!(!receipt.exists());
    case.assert_preserved(&before);
}
fn python_read_capability(path: &Path) -> PathBuf {
    assert!(path.is_absolute());
    let path = path.canonicalize().unwrap();
    assert_ne!(path, Path::new("/"));
    path
}
#[test]
fn python_fixture_read_grants_refuse_canonical_root() {
    let root = tempfile::tempdir().unwrap();
    let link = root.path().join("root-link");
    std::os::unix::fs::symlink("/", &link).unwrap();
    for path in [Path::new("/"), Path::new("/tmp/.."), link.as_path()] {
        assert!(std::panic::catch_unwind(|| python_read_capability(path)).is_err());
    }
    assert_eq!(
        python_read_capability(root.path()),
        root.path().canonicalize().unwrap()
    );
}
fn grant_python(case: &mut Case) {
    let python =
        PathBuf::from(std::env::var_os("FORGE_TEST_PYTHON").expect("explicit Python with pytest"));
    let read = python
        .parent()
        .and_then(Path::parent)
        .expect("Python needs a bounded read root");
    assert_ne!(read, Path::new("/"));
    case.grants["read_only"]
        .as_array_mut()
        .unwrap()
        .push(json!(read));
    if let Some(extra) = std::env::var_os("FORGE_TEST_PYTHON_READ_ONLY") {
        let paths: Vec<_> = std::env::split_paths(&extra).collect();
        assert!(paths.len() <= 16);
        for path in paths {
            case.grants["read_only"]
                .as_array_mut()
                .unwrap()
                .push(json!(python_read_capability(&path)));
        }
    }
    case.grants["programs"]["python"] = json!(python);
    case.grants["python"] = json!("python");
    case.grants["resources"] = json!({"cpu_millis":1000,"memory_mib":512,"processes":32});
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":1024,"processes":64});
    case.grants["max_wall_seconds"] = json!(30);
    case.grants["max_cpu_seconds"] = json!(30);
    case.grants["max_address_space_mib"] = json!(512);
}
#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT, FORGE_TEST_BIN, FORGE_TEST_PYTHON with pytest, and established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn invalid_pytest_configuration_refuses_before_any_review() {
    let mut case = Case::new(Path::new("/bin/true"));
    case.delegated();
    grant_python(&mut case);
    case.fixture.stage(
        "pytest.ini",
        "[pytest]\naddopts = --definitely-not-a-pytest-option\n",
    );
    stage_corpus(&case);
    let base = commit(&case);
    let before = case.source_state();
    let receipt = case.fixture.root.join("must-not-exist.json");
    let mut command = command(&case, &base, &receipt, false);
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(70), 1024 * 1024).unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", diagnostics(&output));
    let gate: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid gate JSON: {error}; {}", diagnostics(&output)));
    let phase = pytest_phase(&gate, &output);
    assert_eq!(
        phase["ok"],
        false,
        "phase={phase:#}\nreport={gate:#}\n{}",
        diagnostics(&output)
    );
    assert!(
        phase["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("definitely-not-a-pytest-option")),
        "phase={phase:#}\nreport={gate:#}\n{}",
        diagnostics(&output)
    );
    assert!(!receipt.exists());
    case.assert_preserved(&before);
}
fn version_case(expected: &str) -> (Case, String) {
    let mut case = Case::new(Path::new("/usr/bin/printf"));
    case.delegated();
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":512,"processes":32});
    case.staged_policy(|policy|format!("{policy}\n[tools.printf]\nexecutable='worker'\nversion_command=['worker','fixture-v1']\nexpected_version='{expected}'\nrequired_profiles=['fast']\nprovided_by='fixture'\nrationale='fixture'\n"));
    stage_corpus(&case);
    let base = commit(&case);
    (case, base)
}
#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT, FORGE_TEST_BIN, and established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn granted_version_probe_runs_and_skip_review_is_reported() {
    let (case, base) = version_case("fixture-v1");
    let before = case.source_state();
    let receipt = case.fixture.root.join("skipped-review.json");
    let mut command = command(&case, &base, &receipt, true);
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(70), 1024 * 1024).unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let gate: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(gate["review_skipped"], true);
    assert!(!gate["phases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["name"] == "review"));
    assert_eq!(gate["tools"][0]["version"], "fixture-v1");
    assert_eq!(gate["tools"][0]["matches_expected"], true);
    assert!(!receipt.exists());
    case.assert_preserved(&before);
}
#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT, FORGE_TEST_BIN, and established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn granted_version_drift_refuses_before_corpus_and_review() {
    let (case, base) = version_case("wrong-version");
    let before = case.source_state();
    let receipt = case.fixture.root.join("drift-review.json");
    let mut command = command(&case, &base, &receipt, false);
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(60), 1024 * 1024).unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let gate: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(gate["tools"][0]["matches_expected"], false);
    assert!(gate["phases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["name"] == "tool-preflight" && row["ok"] == false));
    assert!(!receipt.exists());
    case.assert_preserved(&before);
}

#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT, FORGE_TEST_BIN, FORGE_TEST_PYTHON with pytest, and established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn valid_pytest_configuration_collects_before_explicit_review_skip() {
    let mut case = Case::new(Path::new("/bin/true"));
    case.delegated();
    grant_python(&mut case);
    case.fixture.stage("pytest.ini", "[pytest]\naddopts = -q\n");
    case.fixture.stage(
        "conftest.py",
        &format!(
            "import sys\nassert sys.pycache_prefix\nassert sys.dont_write_bytecode\nassert sys.executable == {}\n",
            serde_json::to_string(&std::env::var("FORGE_TEST_PYTHON").unwrap()).unwrap()
        ),
    );
    stage_corpus(&case);
    let base = commit(&case);
    let before = case.source_state();
    let receipt = case.fixture.root.join("skipped.json");
    let mut command = command(&case, &base, &receipt, true);
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(120), 1024 * 1024).unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", diagnostics(&output));
    let gate: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid gate JSON: {error}; {}", diagnostics(&output)));
    let phase = pytest_phase(&gate, &output);
    assert_eq!(
        phase["ok"],
        true,
        "phase={phase:#}\nreport={gate:#}\n{}",
        diagnostics(&output)
    );
    assert_eq!(
        phase["evidence"]["configs_checked"],
        json!(["pytest.ini"]),
        "phase={phase:#}\nreport={gate:#}\n{}",
        diagnostics(&output)
    );
    assert_eq!(
        gate["review_skipped"],
        true,
        "report={gate:#}\n{}",
        diagnostics(&output)
    );
    assert!(!receipt.exists());
    case.assert_preserved(&before);
}
#[test]
fn installed_native_gate_help_needs_no_python_site_or_runtime_profile() {
    let host = tempfile::tempdir().unwrap();
    let fake_site = host.path().join("site");
    fs::create_dir(&fake_site).unwrap();
    let mut command = std::process::Command::new(support::forge_binary::path());
    command
        .args(["candidate", "gate", "--help"])
        .current_dir(host.path())
        .env_clear()
        .env("PYTHONPATH", &fake_site);
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(10), 128 * 1024).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--git-build-profile"));
    assert!(stdout.contains("--skip-review"));
    assert!(!stdout.contains(fake_site.to_str().unwrap()));
}
#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT, FORGE_TEST_BIN, and established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn tool_required_only_for_full_profile_does_not_block_fast_gate() {
    let mut case = Case::new(Path::new("/bin/true"));
    case.delegated();
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":512,"processes":32});
    case.staged_policy(|policy|format!("{policy}\n[tools.full_only]\nexecutable='not-granted'\nversion_command=['not-granted','--version']\nexpected_version='1'\nrequired_profiles=['full']\nprovided_by='fixture'\nrationale='fixture'\n"));
    stage_corpus(&case);
    let base = commit(&case);
    let before = case.source_state();
    let receipt = case.fixture.root.join("skipped.json");
    let mut command = command(&case, &base, &receipt, true);
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(70), 1024 * 1024).unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let gate: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(gate["tools"], json!([]));
    assert!(gate["phases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["name"] == "tool-preflight" && row["ok"] == true));
    assert!(!receipt.exists());
    case.assert_preserved(&before);
}
#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT, FORGE_TEST_BIN, and established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn failing_version_command_is_versionless_and_refuses_the_gate() {
    let mut case = Case::new(Path::new("/bin/false"));
    case.delegated();
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":512,"processes":32});
    case.staged_policy(|policy|format!("{policy}\n[tools.failing]\nexecutable='worker'\nversion_command=['worker','--version']\nexpected_version='1'\nrequired_profiles=['fast']\nprovided_by='fixture'\nrationale='fixture'\n"));
    stage_corpus(&case);
    let base = commit(&case);
    let before = case.source_state();
    let receipt = case.fixture.root.join("must-not-exist.json");
    let mut command = command(&case, &base, &receipt, false);
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(60), 1024 * 1024).unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let gate: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(gate["tools"][0]["version"], Value::Null);
    assert_eq!(gate["tools"][0]["matches_expected"], false);
    assert!(!receipt.exists());
    case.assert_preserved(&before);
}
