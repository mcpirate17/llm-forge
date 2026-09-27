//! Synthetic Git repositories and subprocess assertions for Rust contracts.

use std::fs;
use std::path::Path;
use std::process::Command;

pub fn git(repo: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run fixture Git command");
    assert!(
        result.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout)
        .expect("UTF-8 fixture Git output")
        .trim()
        .to_owned()
}

pub fn init_protected_repo(repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-b", "main"]);
    git(
        repo,
        &["config", "user.email", "governance-tests@example.invalid"],
    );
    git(repo, &["config", "user.name", "Governance Tests"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
}

pub fn write(repo: &Path, relative: &str, contents: &str) {
    let file = repo.join(relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, contents).unwrap();
}

pub fn init_snapshot_repo(repo: &Path) {
    fs::create_dir(repo).unwrap();
    git(repo, &["init", "--quiet", "-b", "main"]);
    git(repo, &["config", "user.email", "test@example.invalid"]);
    git(repo, &["config", "user.name", "test"]);
    write(repo, "tracked.py", "VALUE = 1\n");
    git(repo, &["add", "tracked.py"]);
    git(repo, &["commit", "--quiet", "-m", "first"]);
}
