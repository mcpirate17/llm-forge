#![cfg(target_os = "linux")]
mod support;
use git2::Repository;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};
use support::{image, json, Fixture};

#[test]
fn actual_binary_preview_preserves_objects_refs_index_and_worktree() {
    let fixture = Fixture::new();
    fixture.stage_layers();
    fs::write(fixture.root.join("untracked"), b"retained\n").unwrap();
    let before = image(&fixture.root);
    let report = json(&fixture.run(&["--dry-run", "--json"]), 0);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["interface"], "forge.checkout-sync");
    assert_eq!(report["outcome"], "would-fast-forward");
    assert_eq!(report["target"], "refs/remotes/origin/main");
    assert_eq!(report["fast_forwardable"], true);
    assert_eq!(report["snapshot"], serde_json::Value::Null);
    assert_eq!(report["planned_index_paths"], 2);
    assert_eq!(before, image(&fixture.root));
    assert_eq!(fs::read_dir(&fixture.recovery).unwrap().count(), 0);
}
#[test]
fn actual_binary_execute_preserves_staged_and_unstaged_layers_and_inspects_completed_recovery() {
    let fixture = Fixture::new();
    fixture.stage_layers();
    let report = json(&fixture.execute(), 0);
    assert_eq!(report["outcome"], "fast-forwarded");
    assert_eq!(
        fs::read(fixture.root.join("a")).unwrap(),
        b"ONE\ntwo\nTHREE\nfour\nFIVE\n"
    );
    assert_eq!(fs::read(fixture.root.join("b")).unwrap(), b"new\n");
    let repo = Repository::open(&fixture.root).unwrap();
    assert_eq!(repo.head().unwrap().target(), Some(fixture.new));
    let index = repo.index().unwrap();
    assert_eq!(
        repo.find_blob(index.get_path(Path::new("a"), 0).unwrap().id)
            .unwrap()
            .content(),
        b"ONE\ntwo\nTHREE\nfour\nfive\n"
    );
    let snapshot = report["snapshot"].as_str().unwrap();
    assert!(repo.find_reference(snapshot).is_ok());
    let directory = report["recovery_directory"].as_str().unwrap();
    let journal_before = fs::read(Path::new(directory).join("journal.jsonl")).unwrap();
    let recovered = json(
        &fixture
            .command()
            .args(["--recover", directory, "--json"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(recovered["outcome"], "journal-marked-complete");
    assert_eq!(
        journal_before,
        fs::read(Path::new(directory).join("journal.jsonl")).unwrap()
    );
    assert_eq!(repo.head().unwrap().target(), Some(fixture.new));
}
#[test]
fn actual_binary_blocks_untracked_and_ignored_collisions_without_snapshot_writes() {
    for ignored in [false, true] {
        let fixture = Fixture::new();
        let repo = Repository::open(&fixture.root).unwrap();
        let incoming = repo.find_commit(fixture.new).unwrap();
        let tree = incoming.tree().unwrap();
        let mut builder = repo.treebuilder(Some(&tree)).unwrap();
        builder
            .insert("secret", repo.blob(b"incoming\n").unwrap(), 0o100644)
            .unwrap();
        let next_tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let sig = git2::Signature::now("test", "test@example.invalid").unwrap();
        let next = repo
            .commit(None, &sig, &sig, "collision", &next_tree, &[&incoming])
            .unwrap();
        repo.reference("refs/remotes/origin/main", next, true, "fixture")
            .unwrap();
        fs::write(fixture.root.join("secret"), b"peer bytes\n").unwrap();
        if ignored {
            fs::write(fixture.root.join(".gitignore"), b"secret\n").unwrap();
        }
        let before = image(&fixture.root);
        for output in [fixture.run(&["--dry-run", "--json"]), fixture.execute()] {
            let report = json(&output, 3);
            assert_eq!(report["outcome"], "blocked");
            assert_eq!(report["fast_forwardable"], false);
            assert!(!report["blocked_by"].as_array().unwrap().is_empty());
        }
        assert_eq!(before, image(&fixture.root));
        assert_eq!(fs::read_dir(&fixture.recovery).unwrap().count(), 0);
        assert_eq!(repo.head().unwrap().target(), Some(fixture.old));
    }
}
#[test]
fn actual_binary_requires_explicit_private_recovery_parent() {
    let fixture = Fixture::new();
    let before = image(&fixture.root);
    let missing = json(&fixture.run(&["--json"]), 2);
    assert!(missing["error"]
        .as_str()
        .unwrap()
        .contains("--recovery-dir"));
    fs::set_permissions(&fixture.recovery, fs::Permissions::from_mode(0o755)).unwrap();
    let unsafe_parent = json(&fixture.execute(), 2);
    assert!(unsafe_parent["error"]
        .as_str()
        .unwrap()
        .contains("mode-0700"));
    assert_eq!(before, image(&fixture.root));
    assert_eq!(fs::read_dir(&fixture.recovery).unwrap().count(), 0);
}
#[test]
fn actual_binary_reports_ref_and_recovery_errors_and_invalid_option_combinations() {
    let fixture = Fixture::new();
    let missing_ref = json(
        &fixture.run(&["--dry-run", "--target", "refs/heads/missing", "--json"]),
        2,
    );
    assert_eq!(missing_ref["outcome"], "error");
    let missing_recovery = json(
        &fixture
            .command()
            .arg("--recover")
            .arg(fixture.recovery.join("missing"))
            .arg("--json")
            .output()
            .unwrap(),
        2,
    );
    assert_eq!(missing_recovery["outcome"], "error");
    let incompatible = fixture
        .command()
        .arg("--recover")
        .arg(&fixture.recovery)
        .arg("--dry-run")
        .output()
        .unwrap();
    assert_eq!(incompatible.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&incompatible.stderr).contains("cannot be used"));
}
#[test]
fn actual_binary_even_and_ahead_outcomes_are_distinct() {
    let fixture = Fixture::new();
    let repo = Repository::open(&fixture.root).unwrap();
    repo.reference("refs/remotes/origin/main", fixture.old, true, "fixture")
        .unwrap();
    let even = json(&fixture.run(&["--json"]), 0);
    assert_eq!(even["outcome"], "even");
    repo.reference("refs/heads/main", fixture.new, true, "fixture")
        .unwrap();
    let blocked = json(&fixture.run(&["--dry-run", "--json"]), 3);
    assert_eq!(blocked["outcome"], "blocked");
    assert_eq!(blocked["ahead"], 1);
    assert_eq!(blocked["fast_forwardable"], false);
}
#[test]
fn actual_binary_interrupt_while_waiting_for_lease_emits_one_error_and_propagates_signal() {
    use std::{
        io::Read,
        os::{
            fd::AsRawFd,
            unix::{fs::OpenOptionsExt, process::ExitStatusExt},
        },
        process::Stdio,
        time::{Duration, Instant},
    };
    let fixture = Fixture::new();
    let governance = fixture.root.join(".git/governance");
    fs::create_dir(&governance).unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(governance.join("commit-review.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let before = image(&fixture.root);
    let mut child = fixture
        .command()
        .arg("--repo")
        .arg(&fixture.root)
        .args(["--dry-run", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let signals_installed = fs::read_dir(format!("/proc/{}/fd", child.id()))
            .unwrap()
            .map(|entry| entry.unwrap())
            .any(|entry| {
                fs::read_link(entry.path())
                    .is_ok_and(|link| link.to_string_lossy().contains("signalfd"))
            });
        if signals_installed {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("native signal supervisor was not installed");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let status = child.wait().unwrap();
    assert_eq!(status.signal(), Some(libc::SIGTERM));
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(report["outcome"], "error");
    assert!(String::from_utf8_lossy(&stderr).contains("forge checkout-sync"));
    assert_eq!(before, image(&fixture.root));
}
