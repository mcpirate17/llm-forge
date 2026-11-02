#![cfg(target_os = "linux")]
#[path = "support/forge_binary.rs"]
mod forge_binary;
use conductor_native::branch_policy_runtime as core;
use git2::{Oid, Repository, RepositoryInitOptions, Signature, Time};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    os::{
        fd::AsRawFd,
        unix::fs::{symlink, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
const NOW: &str = "2026-11-02T12:00:00+00:00";
const A: &str = "claude/first-20261102";
const B: &str = "claude/second-20261102";
struct Fixture {
    _owned: tempfile::TempDir,
    root: PathBuf,
    base: Oid,
    tools: PathBuf,
}
fn guest() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("branch-guest-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let binary = directory.join("guest");
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/branch_policy_guest.rs");
        let compiler = std::env::var_os("RUSTC").unwrap_or("rustc".into());
        let output = Command::new(compiler)
            .args(["--edition=2021", "-C", "debuginfo=0"])
            .arg(source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        binary
    })
}
impl Fixture {
    fn new() -> Self {
        let owned = tempfile::tempdir().unwrap();
        let root = owned.path().join("repo");
        let tools = owned.path().join("tools");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&tools).unwrap();
        let mut options = RepositoryInitOptions::new();
        options.initial_head("main");
        let repository = Repository::init_opts(&root, &options).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/owned.rs"), "base\n").unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("src/owned.rs")).unwrap();
        index.write().unwrap();
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - 96 * 3600;
        let signature =
            Signature::new("Fixture", "fixture@example.invalid", &Time::new(seconds, 0)).unwrap();
        let base = repository
            .commit(Some("HEAD"), &signature, &signature, "base", &tree, &[])
            .unwrap();
        Self {
            _owned: owned,
            root: root.canonicalize().unwrap(),
            base,
            tools,
        }
    }
    fn repository(&self) -> Repository {
        Repository::open(&self.root).unwrap()
    }
    fn branch(&self, name: &str, unique: bool) {
        let repository = self.repository();
        let base = repository.find_commit(self.base).unwrap();
        if unique {
            let blob = repository.blob(b"feature\n").unwrap();
            let mut tree = repository.treebuilder(Some(&base.tree().unwrap())).unwrap();
            tree.insert("feature.rs", blob, 0o100644).unwrap();
            let tree = repository.find_tree(tree.write().unwrap()).unwrap();
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                - 72 * 3600;
            let signature =
                Signature::new("Fixture", "fixture@example.invalid", &Time::new(seconds, 0))
                    .unwrap();
            repository
                .commit(
                    Some(&format!("refs/heads/{name}")),
                    &signature,
                    &signature,
                    "feature",
                    &tree,
                    &[&base],
                )
                .unwrap();
        } else {
            repository.branch(name, &base, false).unwrap();
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(forge_binary::path());
        command
            .args(["branch-policy", "--repo"])
            .arg(&self.root)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("CONDUCTOR_HOST_ROOT")
            .env_remove("CONDUCTOR_INTEGRATION_BRANCH");
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        forge::process_probe::run(
            &mut self.command(args),
            Duration::from_secs(15),
            1024 * 1024,
        )
        .unwrap()
    }
    fn json(&self, args: &[&str], code: i32) -> Value {
        let output = self.run(args);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn refs(&self) -> BTreeMap<String, Option<Oid>> {
        self.repository()
            .references()
            .unwrap()
            .map(|reference| {
                let reference = reference.unwrap();
                (reference.name().unwrap().into(), reference.target())
            })
            .collect()
    }
    fn gh(&self) {
        fs::copy(guest(), self.tools.join("gh")).unwrap();
    }
    fn exposed(&self, mode: &str) -> Value {
        self.gh();
        let mut command = self.command(&["exposed", "--json"]);
        command
            .env("PATH", format!("{}:/usr/bin", self.tools.display()))
            .env("FORGE_BRANCH_GH_MODE", mode);
        let output =
            forge::process_probe::run(&mut command, Duration::from_secs(20), 1024 * 1024).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
#[test]
fn names_and_atomic_binding_lifecycle_have_native_cli_contracts() {
    let fixture = Fixture::new();
    let before = fixture.refs();
    assert_eq!(
        fixture.json(&["check-branch", A, "--json"], 0)["agent"],
        "claude"
    );
    let refusal = fixture.json(&["check-branch", "Bad name", "--json"], 1);
    assert_eq!(refusal["ok"], false);
    assert!(refusal["reason"].as_str().unwrap().contains("try "));
    assert_eq!(
        fixture.json(&["status", "--json"], 0)["bindings"],
        json!([])
    );
    assert!(!fixture.root.join(".git/governance").exists());
    let bound = fixture.json(&["bind", "--branch", A, "--claim", "c1", "--json"], 0);
    assert_eq!(bound["owner"], "claude");
    assert!(bound["last_push_at"].is_null());
    assert_eq!(
        fixture.json(&["status", "--json"], 0)["bindings"][0]["stale"],
        false
    );
    assert_eq!(
        fixture.json(&["unbind", "--branch", A, "--json"], 0)["removed"],
        true
    );
    assert_eq!(
        fixture.json(&["unbind", "--branch", A, "--json"], 1)["removed"],
        false
    );
    assert_eq!(fixture.refs(), before);
}
#[test]
fn live_claim_fanout_and_owner_refusals_preserve_store_bytes() {
    let fixture = Fixture::new();
    fixture.branch(A, false);
    fixture.branch(B, false);
    fixture.json(&["bind", "--branch", A, "--claim", "c1", "--json"], 0);
    let path = core::binding_store_path(&fixture.root).unwrap();
    let before = fs::read(&path).unwrap();
    let refused = fixture.json(&["bind", "--branch", B, "--claim", "c1", "--json"], 1);
    assert!(refused["reason"]
        .as_str()
        .unwrap()
        .contains("second live branch"));
    assert_eq!(fs::read(&path).unwrap(), before);
    let refused = fixture.json(
        &[
            "bind", "--branch", B, "--claim", "c2", "--owner", "other", "--json",
        ],
        1,
    );
    assert!(refused["reason"]
        .as_str()
        .unwrap()
        .contains("does not match owner"));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fixture.refs().len(), 3);
}
#[test]
fn dead_binding_does_not_block_claim_reuse_and_six_hour_boundary_is_exact() {
    let fixture = Fixture::new();
    core::bind(
        &fixture.root,
        A,
        "c1",
        "claude",
        "2026-11-02T06:00:00+00:00",
    )
    .unwrap();
    assert_eq!(
        core::status(&fixture.root, NOW).unwrap()["bindings"][0]["stale"],
        false
    );
    assert_eq!(
        core::status(&fixture.root, "2026-11-02T12:00:01+00:00").unwrap()["bindings"][0]["stale"],
        true
    );
    core::bind(
        &fixture.root,
        A,
        "c1",
        "claude",
        "2026-11-02T09:19:30+00:00",
    )
    .unwrap();
    assert_eq!(
        core::status(&fixture.root, NOW).unwrap()["bindings"][0]["age_hours"],
        2.67
    );
    core::bind(&fixture.root, B, "c1", "claude", NOW).unwrap();
    assert_eq!(core::load_bindings(&fixture.root).unwrap().len(), 2);
}
#[test]
fn malformed_store_symlinks_and_writer_lock_refuse_without_modification() {
    let fixture = Fixture::new();
    fixture.json(&["bind", "--branch", A, "--claim", "c1", "--json"], 0);
    let path = core::binding_store_path(&fixture.root).unwrap();
    let original = fs::read(&path).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.with_file_name(".branch-bindings.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    fixture.json(&["bind", "--branch", B, "--claim", "c2", "--json"], 1);
    assert_eq!(fs::read(&path).unwrap(), original);
    drop(lock);
    fs::write(&path, "not JSON").unwrap();
    assert_eq!(fixture.run(&["status"]).status.code(), Some(2));
    assert_eq!(fs::read_to_string(&path).unwrap(), "not JSON");
    fs::remove_file(&path).unwrap();
    let external = fixture._owned.path().join("external");
    fs::write(&external, &original).unwrap();
    symlink(&external, &path).unwrap();
    assert_eq!(fixture.run(&["status"]).status.code(), Some(2));
    assert_eq!(fs::read(&external).unwrap(), original);
}
#[test]
fn audit_keeps_divergence_naming_and_live_fanout_findings() {
    let fixture = Fixture::new();
    fixture.branch(A, true);
    fixture.branch(B, true);
    fixture.branch("misnamed", false);
    let repository = fixture.repository();
    let feature = repository
        .find_reference(&format!("refs/heads/{A}"))
        .unwrap()
        .target()
        .unwrap();
    repository
        .reference("refs/remotes/origin/main", feature, true, "fixture")
        .unwrap();
    let path = core::binding_store_path(&fixture.root).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
    let rows=[A,B].map(|name|json!({"branch":name,"claim_id":"same","owner":"claude","created_at":NOW,"last_push_at":null,"pr_number":null}));
    fs::write(
        &path,
        json!({"schema_version":1,"bindings":rows}).to_string(),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fixture.refs();
    let findings = fixture.json(&["audit", "--json"], 1)["findings"]
        .as_array()
        .unwrap()
        .clone();
    assert!(findings[0].as_str().unwrap().starts_with("rule 1:"));
    assert!(findings
        .iter()
        .any(|row| row.as_str().unwrap().starts_with("rule 2:")));
    assert!(findings
        .last()
        .unwrap()
        .as_str()
        .unwrap()
        .starts_with("rule 3:"));
    assert_eq!(fixture.refs(), before);
}
#[test]
fn exposure_preserves_claim_coverage_and_literal_dirty_paths_without_writes() {
    let fixture = Fixture::new();
    let claim = forge::ownership_lifecycle::create(
        &fixture.root,
        forge::ownership_lifecycle::Create {
            owner: "test-lane",
            paths: &["src/owned.rs".into()],
            justification: "fixture",
            expected_minutes: 60.0,
            max_minutes: 120.0,
        },
    )
    .unwrap();
    let binding_path = core::binding_store_path(&fixture.root).unwrap();
    assert_eq!(
        fs::metadata(binding_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0,
        "native claim creation must leave private metadata usable by branch policy"
    );
    fs::write(fixture.root.join("src/owned.rs"), "dirty\n").unwrap();
    fs::write(fixture.root.join("space name.rs"), "untracked\n").unwrap();
    let old = SystemTime::now() - Duration::from_secs(48 * 3600);
    for name in ["src/owned.rs", "space name.rs"] {
        fs::File::open(fixture.root.join(name))
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
    }
    let refs = fixture.refs();
    let report = fixture.exposed("empty");
    assert!(report["gh_available"].as_bool().unwrap());
    let files = report["stale_dirty_files"].as_array().unwrap();
    assert!(files.iter().any(|row| row["path"] == "space name.rs"));
    assert!(files
        .iter()
        .any(|row| row["path"] == "src/owned.rs" && row["claim_id"] == claim.claim_id));
    assert_eq!(
        report["local_only_commit_exposure"][0]["claim_id"],
        claim.claim_id
    );
    assert_eq!(fixture.refs(), refs);
    assert_eq!(
        fs::read_to_string(fixture.root.join("src/owned.rs")).unwrap(),
        "dirty\n"
    );
}
#[test]
fn optional_pr_failures_are_reported_and_never_invent_absent_pr_evidence() {
    let fixture = Fixture::new();
    fixture.branch(A, true);
    let report = fixture.exposed("fail");
    assert!(!report["gh_errors"].as_array().unwrap().is_empty());
    for row in report["stale_feature_branches"].as_array().unwrap() {
        assert!(!row["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason.as_str().unwrap().contains("no PR")));
    }
    let success = fixture.exposed("empty");
    assert!(success["stale_feature_branches"][0]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason.as_str().unwrap().contains("no PR")));
}
#[test]
fn pushed_and_snapshot_commit_exclusions_and_runtime_config_stay_live() {
    let fixture = Fixture::new();
    let repository = fixture.repository();
    repository
        .reference("refs/snapshots/safe", fixture.base, true, "fixture")
        .unwrap();
    assert!(core::local_only_commits(&fixture.root, "HEAD")
        .unwrap()
        .is_empty());
    fs::write(
        fixture.root.join("pyproject.toml"),
        "[tool.conductor]\nintegration_branch='main'\nretired_integration_branches=['old']\n",
    )
    .unwrap();
    assert_eq!(
        core::integration_branches(&fixture.root).unwrap(),
        ["main", "old"]
    );
    fs::write(
        fixture.root.join("pyproject.toml"),
        "[tool.conductor]\nintegration_branch=3\n",
    )
    .unwrap();
    assert_eq!(fixture.run(&["audit"]).status.code(), Some(2));
}
fn lease_fixture(path: &Path, expires: &str) {
    fs::write(path,json!({"schema":"worktree-lease.v1","owner":"peer","purpose":"live fixture","expires_at":expires}).to_string()).unwrap();
}

#[test]
fn read_only_reap_preview_preserves_primary_locked_leased_and_untrusted_trees() {
    let fixture = Fixture::new();
    fs::copy(guest(), fixture.tools.join("git")).unwrap();
    let secondary = fixture._owned.path().join("secondary");
    fs::create_dir(&secondary).unwrap();
    fs::write(
        secondary.join(".git"),
        "gitdir: fixture-only-not-registered\n",
    )
    .unwrap();
    fs::write(secondary.join("owned-marker"), "preserve").unwrap();
    let inventory = fixture._owned.path().join("inventory");
    let render = |locked: bool| {
        format!(
            "worktree {}\0HEAD {}\0branch refs/heads/main\0\0worktree {}\0HEAD {}\0{}\0",
            fixture.root.display(),
            fixture.base,
            secondary.display(),
            fixture.base,
            if locked { "locked fixture" } else { "" }
        )
    };
    let run = || {
        let mut command = fixture.command(&["exposure-data", "reap-preview"]);
        command
            .env("PATH", &fixture.tools)
            .env("FORGE_BRANCH_WORKTREE_INVENTORY", &inventory);
        let output =
            forge::process_probe::run(&mut command, Duration::from_secs(10), 1024 * 1024).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    fs::write(&inventory, render(false)).unwrap();
    let rows = run();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert!(rows[0]["reason"]
        .as_str()
        .unwrap()
        .contains("contained in main"));
    fs::write(&inventory, render(true)).unwrap();
    assert!(run().as_array().unwrap().is_empty());
    fs::write(&inventory, render(false)).unwrap();
    let lease = secondary.join(".worktree-lease.json");
    lease_fixture(&lease, "9999-01-01T00:00:00+00:00");
    assert!(run().as_array().unwrap().is_empty());
    for invalid in [
        "2020-02-31T00:00:00+00:00",
        "2021-02-29T00:00:00Z",
        "2020-04-31T00:00:00",
    ] {
        lease_fixture(&lease, invalid);
        assert!(
            run().as_array().unwrap().is_empty(),
            "impossible lease date exposed eligible tree: {invalid}"
        );
    }
    lease_fixture(&lease, "2020-02-29T00:00:00+00:00");
    let valid_expired = run();
    assert_eq!(valid_expired.as_array().unwrap().len(), 1);
    assert!(valid_expired[0]["reason"]
        .as_str()
        .unwrap()
        .contains("lease from peer expired"));
    fs::write(&lease, "broken").unwrap();
    assert!(run().as_array().unwrap().is_empty());
    assert_eq!(
        fs::read_to_string(secondary.join("owned-marker")).unwrap(),
        "preserve"
    );
}

#[test]
fn concurrent_claim_binding_writers_have_one_winner_and_no_lost_update() {
    let fixture = Fixture::new();
    fixture.branch(A, false);
    fixture.branch(B, false);
    let refs = fixture.refs();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers = [A, B]
        .into_iter()
        .map(|name| {
            let root = fixture.root.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                core::bind(&root, name, "same", "claude", NOW)
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap().is_ok())
            .filter(|won| *won)
            .count(),
        1
    );
    assert_eq!(core::load_bindings(&fixture.root).unwrap().len(), 1);
    assert_eq!(fixture.refs(), refs);
    let directory = core::binding_store_path(&fixture.root)
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    assert!(!fs::read_dir(directory).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("branch-pending")));
}

#[test]
fn merge_commit_claim_coverage_uses_git_combined_diff_presentation() {
    let fixture = Fixture::new();
    fixture.branch(A, true);
    let repository = fixture.repository();
    let base = repository.find_commit(fixture.base).unwrap();
    let first = repository
        .find_reference(&format!("refs/heads/{A}"))
        .unwrap()
        .peel_to_commit()
        .unwrap();
    let blob = repository.blob(b"owned from other parent\n").unwrap();
    let mut source = repository.treebuilder(Some(&base.tree().unwrap())).unwrap();
    // Replace the nested src tree rather than introducing an invalid slash-named entry.
    let source_tree = base
        .tree()
        .unwrap()
        .get_path(Path::new("src"))
        .unwrap()
        .to_object(&repository)
        .unwrap()
        .peel_to_tree()
        .unwrap();
    let mut nested = repository.treebuilder(Some(&source_tree)).unwrap();
    nested.insert("owned.rs", blob, 0o100644).unwrap();
    source
        .insert("src", nested.write().unwrap(), 0o040000)
        .unwrap();
    let other_tree = repository.find_tree(source.write().unwrap()).unwrap();
    let signature = Signature::now("Fixture", "fixture@example.invalid").unwrap();
    let other_oid = repository
        .commit(
            None,
            &signature,
            &signature,
            "other parent",
            &other_tree,
            &[&base],
        )
        .unwrap();
    let other = repository.find_commit(other_oid).unwrap();
    let mut merged = repository
        .treebuilder(Some(&first.tree().unwrap()))
        .unwrap();
    merged
        .insert("src", nested.write().unwrap(), 0o040000)
        .unwrap();
    let merged = repository.find_tree(merged.write().unwrap()).unwrap();
    let merged_oid = repository
        .commit(
            None,
            &signature,
            &signature,
            "merge-only",
            &merged,
            &[&first, &other],
        )
        .unwrap();
    repository
        .reference("refs/heads/main", merged_oid, true, "fixture")
        .unwrap();
    forge::ownership_lifecycle::create(
        &fixture.root,
        forge::ownership_lifecycle::Create {
            owner: "merge-owner",
            paths: &["src/owned.rs".into()],
            justification: "fixture",
            expected_minutes: 60.0,
            max_minutes: 120.0,
        },
    )
    .unwrap();
    let report = fixture.exposed("empty");
    let merge = report["local_only_commit_exposure"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["subject"] == "merge-only")
        .unwrap();
    assert_eq!(merge["claim_id"], "UNCLAIMED");
}
