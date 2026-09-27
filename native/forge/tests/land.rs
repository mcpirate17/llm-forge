//! `forge land` end to end against a real bare remote: a green branch lands
//! as a fast-forward and is deleted; a red blocking check, a missing
//! trailer, or a branch loosening its own gate leaves the target untouched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    root: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=master",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn config(clone: &Path, check: &str) -> String {
    format!(
        "target = \"master\"\nclone_dir = \"{}\"\nrequired_trailers = [\"Agent\"]\n\n\
         [[check]]\nname = \"gate\"\nrun = '{check}'\npaths = ['\\.txt$']\ntimeout_s = 30\n\n\
         [[check]]\nname = \"debt\"\nrun = \"exit 9\"\ntimeout_s = 30\nblocking = false\n",
        clone.display()
    )
}

impl Fixture {
    /// A bare remote whose master carries a land config running `check`,
    /// and a work checkout of it.
    fn new(check: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "forge-land-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet", "--bare", "remote.git"]);
        git(&root, &["clone", "--quiet", "remote.git", "work"]);
        let fixture = Self { root };
        let work = fixture.work();
        // forge land must carry this identity into its clone: the runs
        // below see no global git config.
        git(&work, &["config", "user.name", "t"]);
        git(&work, &["config", "user.email", "t@t"]);
        std::fs::create_dir_all(work.join(".forge")).unwrap();
        std::fs::write(
            work.join(".forge/land.toml"),
            config(&fixture.clone_dir(), check),
        )
        .unwrap();
        fixture.commit(".forge/land.toml", None, "chore: land config\n\nAgent: t");
        git(&work, &["push", "--quiet", "origin", "HEAD:master"]);
        fixture
    }
    fn work(&self) -> PathBuf {
        self.root.join("work")
    }
    fn clone_dir(&self) -> PathBuf {
        self.root.join("land/clone")
    }
    fn commit(&self, path: &str, body: Option<&str>, message: &str) {
        let work = self.work();
        if let Some(body) = body {
            std::fs::write(work.join(path), body).unwrap();
        }
        git(&work, &["add", path]);
        git(&work, &["commit", "--quiet", "-m", message]);
    }
    /// Commits `file` on `branch` off the remote master and pushes it.
    fn push_branch(&self, branch: &str, file: &str, message: &str) {
        let work = self.work();
        git(&work, &["fetch", "--quiet", "origin"]);
        git(
            &work,
            &["checkout", "--quiet", "-B", branch, "origin/master"],
        );
        self.commit(file, Some(branch), message);
        git(
            &work,
            &["push", "--quiet", "origin", &format!("HEAD:{branch}")],
        );
    }
    fn land(&self, branch: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_forge"))
            .args(["land", branch])
            .current_dir(self.work())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL")
            .env_remove("EMAIL")
            .output()
            .unwrap()
    }
    fn remote_ref(&self, name: &str) -> Option<String> {
        let out = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", name])
            .current_dir(self.root.join("remote.git"))
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_green_branch_fast_forwards_the_target_and_is_deleted() {
    let f = Fixture::new("grep -q feat a.txt");
    f.push_branch("feat", "a.txt", "feat: a\n\nAgent: t");
    // Master moves after the branch forks: landing must rebase, which
    // writes commits and so needs the invoker's identity in the clone.
    f.push_branch("side", "b.txt", "feat: side\n\nAgent: t");
    git(&f.work(), &["push", "--quiet", "origin", "side:master"]);
    git(&f.work(), &["push", "--quiet", "origin", ":side"]);
    let before = f.remote_ref("master").unwrap();
    let out = f.land("feat");
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("(advisory)"),
        "the advisory failure is reported: {}",
        text(&out)
    );
    let after = f.remote_ref("master").unwrap();
    assert_ne!(before, after);
    assert!(
        f.remote_ref("feat").is_none(),
        "the landed branch is deleted"
    );
    let log = git(
        &f.root.join("remote.git"),
        &["log", "--format=%s", "master"],
    );
    assert_eq!(
        log.lines().take(2).collect::<Vec<_>>(),
        ["feat: a", "feat: side"]
    );
}

#[test]
fn a_red_blocking_check_leaves_the_target_and_branch_alone() {
    let f = Fixture::new("exit 4");
    f.push_branch("feat", "a.txt", "feat: a\n\nAgent: t");
    let before = f.remote_ref("master");
    let out = f.land("feat");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("FAIL  gate exit 4"));
    assert_eq!(f.remote_ref("master"), before);
    assert!(f.remote_ref("feat").is_some());
}

#[test]
fn a_missing_trailer_is_refused_before_any_check() {
    let f = Fixture::new("true");
    f.push_branch("feat", "a.txt", "feat: no trailer");
    let out = f.land("feat");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(text(&out).contains("no Agent:"));
    assert!(!text(&out).contains("pass  gate"));
}

#[test]
fn a_branch_cannot_loosen_its_own_gate() {
    let f = Fixture::new("exit 4");
    let work = f.work();
    git(
        &work,
        &["checkout", "--quiet", "-B", "loosen", "origin/master"],
    );
    std::fs::write(
        work.join(".forge/land.toml"),
        config(&f.clone_dir(), "true"),
    )
    .unwrap();
    f.commit(".forge/land.toml", None, "chore: loosen\n\nAgent: t");
    f.commit("a.txt", Some("x"), "feat: a\n\nAgent: t");
    git(&work, &["push", "--quiet", "origin", "HEAD:loosen"]);
    // The checkout now carries the loosened copy; the target's copy governs.
    let out = f.land("loosen");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
}
