use super::*;
use crate::candidate_mutation_runner::{FilePin, Manifest, BINDINGS, COMPONENTS};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn write(root: &Path, path: &str, bytes: &[u8]) {
    let target = root.join(path);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, bytes).unwrap();
}
const POLICY:&str = "schema_version=1\nblock_at='high'\nmax_workers=2\ncache_ttl_days=7\nclaim_max_age_hours=24\nmax_file_bytes=100000\nmax_binary_bytes=200000\ncoverage_threshold=80.0\nhigh_risk_coverage_threshold=90.0\nbaseline_expires=2099-11-30\n[classes]\npython=['**/*.py']\ntest=['tests/**']\nnative=['**/*.rs']\n[risk]\nhigh=[]\n[paths]\nprotected_deletes=[]\nhot=[]\ngenerated=[]\n[checks.mutation-evidence]\nkind='builtin'\nprofiles=['fast']\nalways=true\n";
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    scratch: PathBuf,
    bundle: PathBuf,
    bundle_hash: String,
    git: PathBuf,
    git_hash: String,
    profile: PathBuf,
    profile_hash: String,
}
fn fixture_git_profile(
    test_git: Option<PathBuf>,
    private: &Path,
    temp: &Path,
) -> (PathBuf, String, PathBuf, String) {
    let fake = test_git.is_some();
    let git = test_git.unwrap_or_else(|| {
        std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|p| p.join("git"))
            .find(|p| p.is_file())
            .unwrap()
            .canonicalize()
            .unwrap()
    });
    let git_hash = hash(&fs::read(&git).unwrap());
    let (profile, profile_hash) = if fake {
        let bytes = serde_json::to_vec(&serde_json::json!({"schema_version":1,
            "git_sha256":git_hash,"system_attributes":{"mode":"none"}}))
        .unwrap();
        let path = private.join("profile.json");
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        (path, hash(&bytes))
    } else {
        crate::mutation_corpus_test_support::profile(temp, &git_hash)
    };
    (git, git_hash, profile, profile_hash)
}

impl Fixture {
    fn new() -> Self {
        Self::new_with_git(None)
    }
    fn new_with_git(test_git: Option<PathBuf>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let scratch = tmp.path().join("scratch");
        let package = tmp.path().join("installed");
        let private = tmp.path().join("private");
        for path in [&scratch, &package, &private] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let repo = git2::Repository::init(&root).unwrap();
        write(
            &root,
            "forge.toml",
            b"schema_version=1\n[conductor]\nmutation_registry='campaigns/registry.json'\n",
        );
        write(&root, "policy.toml", POLICY.as_bytes());
        let source = b"def subject():\n    return 1\n";
        let tests = b"def test_subject():\n    assert True\n";
        write(&root, "pkg/subject.py", source);
        write(&root, "pkg/test_subject.py", tests);
        let registry = serde_json::json!({"schema_version":1,"enforcement":"changed_tests", "test_patterns":conductor_native::mutation_evidence::canonical_test_patterns(),
            "receipt_directories":["campaigns/receipts"],"campaigns":[{"manifest":"campaigns/campaign.json"}]});
        write(
            &root,
            "campaigns/registry.json",
            serde_json::to_vec(&registry).unwrap().as_slice(),
        );
        let manifest = serde_json::json!({"schema_version":1,"campaign_id":"subject", "title":"Subject", "language":"python", "mutation_engine":"fest",
            "generator":{"engine":"fest","source":["pkg/subject.py"],"run_timeout_seconds":900,"jobs":1},
            "source_sha256":{"pkg/subject.py":hash(source)}, "test_sha256":{"pkg/test_subject.py":hash(tests)},
            "test_argv":["python","-m","pytest","pkg/test_subject.py"],"survivor_baseline":[]});
        write(
            &root,
            "campaigns/campaign.json",
            serde_json::to_vec(&manifest).unwrap().as_slice(),
        );
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Fixture", "test@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
            .unwrap();
        let mut components = BTreeMap::new();
        let mut bindings = BTreeMap::new();
        for (i, key) in COMPONENTS.iter().chain(BINDINGS.iter()).enumerate() {
            let path = package.join(format!("file-{i}"));
            fs::write(&path, key.as_bytes()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let pin = FilePin {
                path,
                sha256: hash(key.as_bytes()),
            };
            if i < COMPONENTS.len() {
                components.insert((*key).into(), pin);
            } else {
                bindings.insert((*key).into(), pin);
            }
        }
        let bundle = private.join("bundle.json");
        let bytes = serde_json::to_vec(&Manifest {
            schema_version: 1,
            symbol_hash_protocol: "cpython-ast-dump-3.12".into(),
            package_root: package,
            components,
            bindings,
            lineage: None,
            external_invariant_probe: None,
        })
        .unwrap();
        fs::write(&bundle, &bytes).unwrap();
        fs::set_permissions(&bundle, fs::Permissions::from_mode(0o600)).unwrap();
        let (git, git_hash, profile, profile_hash) =
            fixture_git_profile(test_git, &private, tmp.path());
        Self {
            _tmp: tmp,
            root,
            scratch,
            bundle,
            bundle_hash: hash(&bytes),
            git,
            git_hash,
            profile,
            profile_hash,
        }
    }
    fn args(&self, record: bool) -> AuditArgs {
        AuditArgs {
            repo: self.root.clone(),
            kind: Some(Kind::Commit),
            target_ref: "HEAD".into(),
            base_ref: None,
            policy: Some("policy.toml".into()),
            scratch_parent: Some(self.scratch.clone()),
            git: self.git.clone(),
            git_sha256: self.git_hash.clone(),
            git_build_profile: self.profile.clone(),
            git_build_profile_sha256: self.profile_hash.clone(),
            mutation_runner_bundle: self.bundle.clone(),
            mutation_runner_sha256: self.bundle_hash.clone(),
            registry: Some("campaigns/registry.json".into()),
            baseline: None,
            changed_file: vec![],
            changed_files_from: None,
            max_workers: 16,
            summary: false,
            write_baseline: record,
        }
    }
}
#[test]
#[ignore = "requires operator-established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn corpus_cli_finds_uncovered_campaign_and_explicitly_records_baseline() {
    let _owner = crate::process_signals::TEST_OWNER.lock().unwrap();
    let f = Fixture::new();
    assert_eq!(run(f.args(false)).unwrap(), 7);
    assert_eq!(run(f.args(true)).unwrap(), 0);
    let baseline: serde_json::Value = serde_json::from_slice(
        &fs::read(f.root.join("campaigns/reproducibility_baseline.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        baseline["uncovered_campaigns"],
        serde_json::json!(["subject"])
    );
}

#[test]
#[ignore = "requires operator-established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn live_cli_observes_dirty_manifest_and_untracked_receipt_inputs() {
    let _owner = crate::process_signals::TEST_OWNER.lock().unwrap();
    let f = Fixture::new();
    let path = f.root.join("campaigns/campaign.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["campaign_id"] = serde_json::json!("dirty");
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let receipt = f.root.join("campaigns/receipts/untracked.json");
    fs::create_dir_all(receipt.parent().unwrap()).unwrap();
    fs::write(&receipt, br#"{"campaign_id":"dirty","status":"FAIL"}"#).unwrap();
    let before_index = fs::read(f.root.join(".git/index")).unwrap();
    let mut args = f.args(true);
    args.kind = None;
    args.policy = None;
    assert_eq!(run(args).unwrap(), 0);
    let baseline: serde_json::Value = serde_json::from_slice(
        &fs::read(f.root.join("campaigns/reproducibility_baseline.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        baseline["uncovered_campaigns"],
        serde_json::json!(["dirty"])
    );
    assert_eq!(fs::read(f.root.join(".git/index")).unwrap(), before_index);
}
fn add_legacy_campaign(f: &Fixture, id: &str, test_value: bool) {
    let source = fs::read(f.root.join("pkg/subject.py")).unwrap();
    let tests = fs::read(f.root.join("pkg/test_subject.py")).unwrap();
    let patch = b"diff --git a/pkg/subject.py b/pkg/subject.py\n--- a/pkg/subject.py\n+++ b/pkg/subject.py\n@@ -1,2 +1,2 @@\n def subject():\n-    return 9\n+    return 2\n";
    write(&f.root, &format!("campaigns/{id}.patch"), patch);
    let node = "pkg/test_subject.py::test_subject";
    let manifest = serde_json::json!({"schema_version":1,"campaign_id":id,"title":"Fixture", "language":"python", "mutation_engine":"reviewed_unified_diff",
        "expected_ranked_tests":1,"expected_mutations":1,"source_sha256":{"pkg/subject.py":hash(&source),"pkg/test_subject.py":hash(&tests)},
        "ranked_tests":[{"rank":1,"nodeid":node,"contract":"subject works","rationale":"fixture"}],
        "planned_mutations":[{"id":"change","target_path":"pkg/subject.py","contract":"result","description":"change result","expected_killers":[node]}],
        "mutations":[{"id":"change","patch_file":format!("{id}.patch"),"patch_sha256":hash(patch),"allowed_paths":["pkg/subject.py"],"expected_killers":[node]}],
        "baseline":{"timeout_seconds":10,"argv":["/nonexistent-fixture-python","-m","pytest",node]},
        "test_scopes":{"pkg/test_subject.py":{"mode":"partial","inventory":"python_ast","nodeids":[node]}}});
    write(
        &f.root,
        &format!("campaigns/{id}.json"),
        &serde_json::to_vec(&manifest).unwrap(),
    );
    if test_value {
        let bundle: Manifest = serde_json::from_slice(&fs::read(&f.bundle).unwrap()).unwrap();
        let components: BTreeMap<_, _> = bundle
            .components
            .into_iter()
            .map(|(key, pin)| (key, pin.sha256))
            .collect();
        let receipt = serde_json::json!({"campaign_id":id,"status":"PASS","generated_at":"2026-01-01",
            "runner_components_sha256":components,"source_sha256":{"pkg/subject.py":hash(&source),"pkg/test_subject.py":hash(&tests)},
            "test_value":{"tests":[{"nodeid":node,"classification":"DELETE_CANDIDATE"}]}});
        write(
            &f.root,
            &format!("campaigns/receipts/{id}.json"),
            &serde_json::to_vec(&receipt).unwrap(),
        );
    }
}
#[test]
#[ignore = "requires operator-established FORGE_TEST_GIT_SYSTEM_ATTRIBUTES"]
fn live_cli_records_all_seven_dimensions_from_real_inputs() {
    let _owner = crate::process_signals::TEST_OWNER.lock().unwrap();
    let f = Fixture::new();
    add_legacy_campaign(&f, "unmeasured", false);
    add_legacy_campaign(&f, "inert", true);
    let path = f.root.join("campaigns/registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for name in ["unmeasured", "inert", "missing"] {
        registry["campaigns"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"manifest":format!("campaigns/{name}.json")}));
    }
    fs::write(path, serde_json::to_vec(&registry).unwrap()).unwrap();
    write(&f.root, "pkg/unpinned.py", b"new source\n");
    let mut args = f.args(true);
    args.kind = None;
    args.policy = None;
    args.changed_file = vec!["pkg/unpinned.py".into()];
    assert_eq!(run(args).unwrap(), 0);
    let baseline: serde_json::Value = serde_json::from_slice(
        &fs::read(f.root.join("campaigns/reproducibility_baseline.json")).unwrap(),
    )
    .unwrap();
    let expected = serde_json::json!({
        "stale_mutations":["inert::change", "unmeasured::change"],
        "unloadable_manifests":["campaigns/missing.json"],
        "host_pinned_interpreters":["inert", "unmeasured"],
        "uncovered_campaigns":["subject", "unmeasured"],
        "campaigns_without_value_analysis":["unmeasured"],
        "tests_that_kill_nothing":["inert::pkg/test_subject.py::test_subject"],
        "uncovered_changed_files":["pkg/unpinned.py"]
    });
    for dimension in conductor_native::mutation_corpus_delta::BASELINE_KEYS {
        assert_eq!(baseline[dimension], expected[dimension], "{dimension}");
    }
    let mut replay = f.args(false);
    replay.kind = None;
    replay.policy = None;
    replay.changed_file = vec!["pkg/unpinned.py".into()];
    assert_eq!(run(replay).unwrap(), 0);
}

#[path = "mutation_corpus_cli_cancel_tests.rs"]
mod cancellation;
