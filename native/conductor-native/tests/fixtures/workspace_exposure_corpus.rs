//! Git recipe builder shared by the native and Python exposure-line contracts.
//! The expected lines remain frozen from the original Python corpus generator.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(cwd: &Path, args: &[&str]) {
    let done = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "parity")
        .env("GIT_AUTHOR_EMAIL", "parity@example.invalid")
        .env("GIT_COMMITTER_NAME", "parity")
        .env("GIT_COMMITTER_EMAIL", "parity@example.invalid")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run fixture git");
    assert!(
        done.status.success(),
        "git {} in {}: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&done.stderr)
    );
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("fixture parent")).unwrap();
    std::fs::write(path, text).unwrap();
}

fn make_seed(parent: &Path, case: &Value) -> PathBuf {
    let id = case["id"].as_str().expect("corpus id");
    let branch = case["branch"].as_str().expect("corpus branch");
    let repo = parent.join(format!("{id}-repo"));
    let origin = parent.join(format!("{id}-origin.git"));
    if case["origin"].as_bool().unwrap_or(false) {
        git(
            parent,
            &[
                "init",
                "--quiet",
                "--bare",
                "-b",
                branch,
                &origin.display().to_string(),
            ],
        );
    }
    git(
        parent,
        &["init", "--quiet", "-b", branch, &repo.display().to_string()],
    );
    git(&repo, &["config", "user.email", "parity@example.invalid"]);
    git(&repo, &["config", "user.name", "parity"]);
    write(&repo.join("seed.txt"), "seed\n");
    if let Some(integration) = case["integration"].as_str() {
        write(
            &repo.join("pyproject.toml"),
            &format!("[tool.conductor]\nintegration_branch = \"{integration}\"\n"),
        );
        git(&repo, &["add", "pyproject.toml"]);
    }
    git(&repo, &["add", "seed.txt"]);
    git(&repo, &["commit", "--quiet", "-m", "seed"]);
    if case["origin"].as_bool().unwrap_or(false) {
        git(
            &repo,
            &["remote", "add", "origin", &origin.display().to_string()],
        );
        git(&repo, &["push", "--quiet", "origin", branch]);
    }
    if let Some(origin_head) = case["origin_head"].as_str() {
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                &format!("refs/remotes/origin/{origin_head}"),
            ],
        );
    }
    repo
}

fn add_files(repo: &Path, case: &Value) {
    for name in case["commits"].as_array().expect("corpus commits") {
        let name = name.as_str().expect("commit name");
        write(&repo.join(name), &format!("{name}\n"));
        git(repo, &["add", name]);
        git(repo, &["commit", "--quiet", "-m", name]);
    }
    for entry in case["files"].as_array().expect("corpus files") {
        let relative = entry["path"].as_str().expect("file path");
        let contents = if entry["kind"] == "modify_tracked" {
            "modified\n"
        } else {
            "untracked\n"
        };
        write(&repo.join(relative), contents);
        if let Some(epoch) = entry["mtime"].as_i64() {
            let done = Command::new("touch")
                .args([
                    "-d",
                    &format!("@{epoch}"),
                    &repo.join(relative).display().to_string(),
                ])
                .output()
                .expect("pin corpus mtime");
            assert!(
                done.status.success(),
                "touch corpus mtime: {}",
                String::from_utf8_lossy(&done.stderr)
            );
        } else {
            assert_eq!(
                entry["mtime"], "now",
                "mtime recipe must be an epoch or now"
            );
        }
    }
}

fn add_worktrees(parent: &Path, repo: &Path, case: &Value) {
    let id = case["id"].as_str().unwrap();
    let branch = case["branch"].as_str().unwrap();
    for (index, row) in case["worktrees"]
        .as_array()
        .expect("corpus worktrees")
        .iter()
        .enumerate()
    {
        let topic = row["branch"].as_str().expect("worktree branch");
        let start = if row["at"] == "pushed" {
            format!("origin/{branch}")
        } else {
            "HEAD".to_owned()
        };
        let tree = parent.join(format!("{id}-wt{index}"));
        git(
            repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                topic,
                &tree.display().to_string(),
                &start,
            ],
        );
        if let Some(name) = row["commit"].as_str() {
            write(&tree.join(name), &format!("{name}\n"));
            git(&tree, &["add", name]);
            git(&tree, &["commit", "--quiet", "-m", name]);
        }
        if row["pruned_upstream"].as_bool().unwrap_or(false) {
            git(&tree, &["push", "--quiet", "-u", "origin", topic]);
            git(&tree, &["push", "--quiet", "origin", "--delete", topic]);
        }
        if row["dirty"].as_bool().unwrap_or(false) {
            write(&tree.join("leftover.txt"), "leftover\n");
        }
    }
}

pub fn build_case(parent: &Path, recipe: &Value) -> PathBuf {
    let repo = make_seed(parent, recipe);
    add_files(&repo, recipe);
    add_worktrees(parent, &repo, recipe);
    repo
}
