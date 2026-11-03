#[path = "support/forge_binary.rs"]
mod forge_binary;
use forge::{
    ownership,
    ownership_lifecycle::{self as lifecycle, Create},
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};
use std::time::Duration;

struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let root = std::env::temp_dir().join(forge::resource_id("claims-test").unwrap());
        fs::create_dir_all(root.join(".git")).unwrap();
        Self(root)
    }
    fn create(&self, owner: &str, path: &str) -> anyhow::Result<ownership::OwnershipClaim> {
        create(&self.0, owner, path)
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn create(root: &Path, owner: &str, path: &str) -> anyhow::Result<ownership::OwnershipClaim> {
    lifecycle::create(
        root,
        Create {
            owner,
            paths: &[path.to_string()],
            justification: "bounded task",
            expected_minutes: 45.0,
            max_minutes: 60.0,
        },
    )
}

#[test]
fn concurrent_overlapping_claims_have_one_winner() {
    let repo = Repo::new();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = ["lane-a", "lane-b"]
        .into_iter()
        .map(|owner| {
            let root = repo.0.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                create(&root, owner, "src/a.rs").is_ok()
            })
        })
        .collect();
    assert_eq!(
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
    assert_eq!(lifecycle::list(&repo.0).unwrap().0.len(), 1);
}
#[test]
fn concurrent_touches_preserve_both_claims_and_wrong_owner_cannot_mutate() {
    let repo = Repo::new();
    let a = repo.create("lane-a", "src/a.rs").unwrap();
    let b = repo.create("lane-b", "src/b.rs").unwrap();
    assert!(lifecycle::release(&repo.0, &a.claim_id, "lane-b").is_err());
    assert!(lifecycle::touch(&repo.0, &a.claim_id, "lane-b").is_err());
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [a, b]
        .into_iter()
        .map(|claim| {
            let root = repo.0.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                lifecycle::touch(&root, &claim.claim_id, &claim.owner).unwrap();
                claim.claim_id
            })
        })
        .collect();
    let ids: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    let seen = ownership::load_activity(&repo.0).unwrap();
    assert!(ids.iter().all(|id| seen.contains_key(id)));
}
#[test]
fn shared_lock_timeout_invalid_paths_and_linked_checkout() {
    let repo = Repo::new();
    let lock = lifecycle::acquire(&repo.0, Duration::ZERO).unwrap();
    assert!(lifecycle::acquire(&repo.0, Duration::from_millis(30)).is_err());
    drop(lock);
    for path in ["../other", "conductor", "src/*", "/etc/passwd", "src/a,b"] {
        assert!(repo.create("lane-a", path).is_err(), "{path}");
    }
    let gitdir = repo.0.join(".git/worktrees/linked");
    let linked = repo.0.join("linked");
    fs::create_dir_all(&gitdir).unwrap();
    fs::create_dir(&linked).unwrap();
    fs::write(gitdir.join("commondir"), "../..\n").unwrap();
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .unwrap();
    let claim = create(&linked, "lane-a", "src/linked.rs").unwrap();
    assert_eq!(
        lifecycle::list(&repo.0).unwrap().0[0].claim_id,
        claim.claim_id
    );
}

#[test]
fn legacy_readable_lock_is_accepted_but_writable_or_linked_locks_are_rejected() {
    let repo = Repo::new();
    let path = repo.0.join(".git/governance/commit-review.lock");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "legacy lease metadata").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    drop(lifecycle::acquire(&repo.0, Duration::ZERO).unwrap());
    assert_eq!(fs::read_to_string(&path).unwrap(), "legacy lease metadata");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(lifecycle::acquire(&repo.0, Duration::ZERO).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let alias = path.with_extension("alias");
    fs::hard_link(&path, &alias).unwrap();
    assert!(lifecycle::acquire(&repo.0, Duration::ZERO).is_err());
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&alias, &path).unwrap();
    assert!(lifecycle::acquire(&repo.0, Duration::ZERO).is_err());
}

fn cli(repo: &Repo, args: &[&str]) -> Output {
    let mut command = Command::new(forge_binary::path());
    command
        .env_clear()
        .env("PATH", "")
        .current_dir(&repo.0)
        .arg("claim")
        .args(args)
        .arg("--repo")
        .arg(&repo.0);
    forge::process_probe::run(&mut command, Duration::from_secs(10), 1024 * 1024).unwrap()
}

#[test]
fn native_cli_lifecycle_works_without_git_or_interpreter_on_path() {
    let repo = Repo::new();
    let create = cli(
        &repo,
        &[
            "create",
            "--owner",
            "lane-a",
            "--justification",
            "native test",
            "src/a.rs",
        ],
    );
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    let listing = cli(&repo, &["list"]);
    assert!(listing.status.success());
    let value: serde_json::Value = serde_json::from_slice(&listing.stdout).unwrap();
    let id = value["claims"][0]["claim_id"].as_str().unwrap();
    assert!(cli(&repo, &["touch", id, "--owner", "lane-a"])
        .status
        .success());
    assert!(!cli(&repo, &["release", id, "--owner", "lane-b"])
        .status
        .success());
    assert!(cli(&repo, &["release", id, "--owner", "LANE-A"])
        .status
        .success());
    assert!(lifecycle::list(&repo.0).unwrap().0.is_empty());
    assert!(ownership::load_activity(&repo.0).unwrap().is_empty());
}

#[test]
fn inherited_lease_environment_cannot_authorize_a_native_mutation() {
    let repo = Repo::new();
    let claim = repo.create("lane-a", "src/a.rs").unwrap();
    let before = fs::read(repo.0.join(".git/governance/ownership-claims.json")).unwrap();
    let mut command = Command::new(forge_binary::path());
    command
        .env_clear()
        .env("PATH", "")
        .env("LLM_GOVERNANCE_COMMIT_LOCK_TOKEN", "unverified-token")
        .args([
            "claim",
            "release",
            &claim.claim_id,
            "--owner",
            "lane-a",
            "--repo",
        ])
        .arg(&repo.0);
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(10), 1024 * 1024).unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be verified"));
    assert_eq!(
        before,
        fs::read(repo.0.join(".git/governance/ownership-claims.json")).unwrap()
    );
}

#[test]
fn claim_report_preserves_legacy_null_without_changing_store_shape() {
    let claim = ownership::OwnershipClaim {
        claim_id: "claim-legacy".into(),
        owner: "lane-a".into(),
        paths: vec!["src/a.rs".into()],
        justification: "legacy".into(),
        created_at: "2026-01-01T00:00:00+00:00".into(),
        expires_at: "2026-01-01T01:00:00+00:00".into(),
        expected_at: None,
        last_seen: Some("2026-01-01T00:01:00+00:00".into()),
    };
    let report = lifecycle::reported_claim(&claim);
    assert!(report.as_object().unwrap().contains_key("expected_at"));
    assert!(report["expected_at"].is_null());
    assert!(report.get("last_seen").is_none());
    assert!(lifecycle::stored_claim(&claim).get("expected_at").is_none());
}

#[test]
fn compact_claim_report_preserves_filter_digest_and_path_visibility() {
    let repo = Repo::new();
    let claim = repo.create("lane-a", "src/a.rs").unwrap();
    let output = cli(&repo, &["list", "--compact", "--path", "src"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("claims: 1 active (0 overrun), 0 expired, sha256 "));
    assert!(text.contains(&claim.claim_id));
    assert!(text.contains("on-time"));
    assert!(text.contains("src/(1)"));
    assert!(text.lines().any(|line| line == "    src/a.rs"));
    let empty = cli(&repo, &["list", "--compact", "--path", "other"]);
    assert!(String::from_utf8(empty.stdout)
        .unwrap()
        .starts_with("claims: 0 active"));
}

#[test]
fn claim_cli_binds_default_owner_to_checkout_and_refuses_other_lanes() {
    let repo = Repo::new();
    fs::write(
        repo.0.join(".git/HEAD"),
        "ref: refs/heads/codex/audit-rust-20260903\n",
    )
    .unwrap();
    let claimed = cli(
        &repo,
        &["create", "--justification", "identity contract", "src/a.rs"],
    );
    assert!(
        claimed.status.success(),
        "{}",
        String::from_utf8_lossy(&claimed.stderr)
    );
    assert!(String::from_utf8_lossy(&claimed.stdout).contains("owner=codex-audit-rust-20260903"));
    let other = cli(
        &repo,
        &[
            "create",
            "--owner",
            "other-lane",
            "--justification",
            "identity contract",
            "src/b.rs",
        ],
    );
    assert!(!other.status.success());
    let vendor = cli(
        &repo,
        &[
            "create",
            "--owner",
            "codex",
            "--justification",
            "identity contract",
            "src/c.rs",
        ],
    );
    assert!(!vendor.status.success());
    assert_eq!(lifecycle::list(&repo.0).unwrap().0.len(), 1);
}

#[test]
fn claims_list_keeps_multi_path_filter_shape_and_compact_truncation() {
    let repo = Repo::new();
    let long = format!("alpha {}", "x".repeat(100));
    let paths = ["src/a.py".to_owned(), "src/b.py".to_owned()];
    lifecycle::create(
        &repo.0,
        Create {
            owner: "alpha",
            paths: &paths,
            justification: &long,
            expected_minutes: 45.0,
            max_minutes: 60.0,
        },
    )
    .unwrap();
    repo.create("beta", "docs.md").unwrap();
    let result = cli(&repo, &["list", "--path", "src/b.py"]);
    assert!(result.status.success());
    let data: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(data["claims"].as_array().unwrap().len(), 1);
    assert_eq!(data["claims"][0]["owner"], "alpha");
    assert!(data["claims"][0].get("expected_at").is_some());
    assert!(data["claims"][0].get("last_seen").is_none());
    let result = cli(&repo, &["list", "--compact", "--path", "src/b.py"]);
    assert!(result.status.success());
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(text.contains("claims: 1 active") && !text.contains("beta"));
    assert!(text.contains("src/(2)"));
    assert!(text.lines().any(|line| line == "    src/a.py src/b.py"));
    let row = text.lines().find(|line| line.contains(" alpha ")).unwrap();
    assert!(row.ends_with('…'));
    assert_eq!(row.rsplit("  ").next().unwrap().chars().count(), 72);
}
