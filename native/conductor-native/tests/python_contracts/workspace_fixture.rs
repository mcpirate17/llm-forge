//! Local Git fixtures shared by the workspace hygiene and reaper contracts.
//! Every Git repository and linked tree is created below `Case::root()`.

use crate::support::path;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn git(cwd: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Reap Test")
        .env("GIT_AUTHOR_EMAIL", "reap@example.invalid")
        .env("GIT_COMMITTER_NAME", "Reap Test")
        .env("GIT_COMMITTER_EMAIL", "reap@example.invalid")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run fixture git");
    assert!(
        result.status.success(),
        "git {} in {}: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

pub fn write(path: &Path, contents: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture parent");
    fs::write(path, contents).expect("write fixture file");
}

pub fn seeded_repo(parent: &Path, name: &str, branch: &str, integration: Option<&str>) -> PathBuf {
    let origin = parent.join(format!("{name}-origin.git"));
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
    let repo = parent.join(name);
    git(
        parent,
        &["init", "--quiet", "-b", branch, &repo.display().to_string()],
    );
    git(&repo, &["config", "user.email", "hygiene@example.invalid"]);
    git(&repo, &["config", "user.name", "hygiene"]);
    write(&repo.join("seed.txt"), "seed\n");
    git(&repo, &["add", "seed.txt"]);
    git(&repo, &["commit", "--quiet", "-m", "seed"]);
    git(
        &repo,
        &["remote", "add", "origin", &origin.display().to_string()],
    );
    git(&repo, &["push", "--quiet", "origin", branch]);
    if let Some(integration) = integration {
        write(
            &repo.join("pyproject.toml"),
            format!("[tool.conductor]\nintegration_branch = \"{integration}\"\n"),
        );
    }
    repo
}

pub fn lineless_repo(parent: &Path, name: &str) -> PathBuf {
    let repo = parent.join(name);
    git(
        parent,
        &[
            "init",
            "--quiet",
            "-b",
            "trunk",
            &repo.display().to_string(),
        ],
    );
    git(&repo, &["config", "user.email", "hygiene@example.invalid"]);
    git(&repo, &["config", "user.name", "hygiene"]);
    write(&repo.join("tracked.txt"), "tracked\n");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "--quiet", "-m", "tracked"]);
    repo
}

pub fn reap_repo(parent: &Path) -> PathBuf {
    let origin = parent.join("origin.git");
    git(
        parent,
        &[
            "init",
            "--quiet",
            "--bare",
            "-b",
            "master",
            &origin.display().to_string(),
        ],
    );
    let repo = parent.join("primary");
    git(
        parent,
        &[
            "clone",
            "--quiet",
            &origin.display().to_string(),
            &repo.display().to_string(),
        ],
    );
    write(&repo.join("seed.txt"), "seed\n");
    git(&repo, &["add", "seed.txt"]);
    git(&repo, &["commit", "--quiet", "-m", "seed"]);
    git(&repo, &["push", "--quiet", "origin", "master"]);
    repo
}

pub fn commit(repo: &Path, name: &str, contents: &str) {
    write(&repo.join(name), contents);
    git(repo, &["add", name]);
    git(repo, &["commit", "--quiet", "-m", name]);
}

pub fn worktree(repo: &Path, name: &str, branch: &str, start: &str) -> PathBuf {
    let tree = repo.parent().expect("repo parent").join(name);
    git(
        repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
            &tree.display().to_string(),
            start,
        ],
    );
    tree
}

pub fn dead_origin_refs(repo: &Path, branches: &[&str]) {
    git(repo, &["remote", "rm", "origin"]);
    git(
        repo,
        &["remote", "add", "origin", "/nonexistent/origin.git"],
    );
    let head = git(repo, &["rev-parse", "HEAD"]);
    for branch in branches {
        git(
            repo,
            &[
                "update-ref",
                &format!("refs/remotes/origin/{branch}"),
                &head,
            ],
        );
    }
}

pub fn set_mtime(path: &Path, seconds_since_epoch: i64) {
    let result = Command::new("touch")
        .args([
            "-d",
            &format!("@{seconds_since_epoch}"),
            &path.display().to_string(),
        ])
        .output()
        .expect("run touch for fixture mtime");
    assert!(
        result.status.success(),
        "touch {}: {}",
        path.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

pub fn age(path: &Path, hours: u64) {
    let then = SystemTime::now() - Duration::from_secs(hours * 3600);
    let epoch = then.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    age_files(path, epoch);
}

fn age_files(path: &Path, epoch: i64) {
    for entry in fs::read_dir(path).expect("walk fixture directory") {
        let entry = entry.expect("read fixture entry");
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path).expect("fixture metadata");
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            age_files(&entry_path, epoch);
        } else if metadata.is_file() {
            set_mtime(&entry_path, epoch);
        }
    }
}

pub fn proc_root(parent: &Path, pid: &str, cwd: Option<&Path>) -> PathBuf {
    let root = parent.join(format!("proc-{pid}"));
    let process = root.join(pid);
    fs::create_dir_all(&process).expect("create synthetic process");
    if let Some(cwd) = cwd {
        symlink(cwd, process.join("cwd")).expect("link synthetic cwd");
    }
    root
}

pub fn decide<'py>(
    py: Python<'py>,
    subject: &Bound<'py, PyModule>,
    repo: &Path,
    current: &Path,
    proc: &Path,
    idle_hours: Option<f64>,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("current", path(py, current)).unwrap();
    kwargs.set_item("proc_root", path(py, proc)).unwrap();
    if let Some(hours) = idle_hours {
        kwargs.set_item("idle_hours", hours).unwrap();
    }
    subject
        .getattr("decide")
        .unwrap()
        .call((path(py, repo),), Some(&kwargs))
        .unwrap()
}

pub fn state<'py>(
    py: Python<'py>,
    decisions: &Bound<'py, PyAny>,
    wanted: &Path,
) -> Bound<'py, PyAny> {
    for result in decisions.try_iter().unwrap() {
        let row = result.unwrap();
        let worktree = row.getattr("worktree").unwrap();
        let found = worktree.getattr("path").unwrap();
        let absolute = found.call_method0("resolve").unwrap();
        if absolute
            .eq(path(py, wanted).call_method0("resolve").unwrap())
            .unwrap()
        {
            return row;
        }
    }
    panic!("{} absent from decisions", wanted.display());
}
