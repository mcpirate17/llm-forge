//! `forge sweep run` end to end against a real bare remote: a green sweep
//! reports, records its head, and the next sweep's range starts there; a red
//! blocking check or a failed setup leaves the swept ref where it was.

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

impl Fixture {
    /// A bare remote whose master carries land and sweep configs, with
    /// `setup` as the land setup and `gate` as the land check the sweep names.
    fn new(setup: &str, gate: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "forge-sweep-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet", "--bare", "remote.git"]);
        git(&root, &["clone", "--quiet", "remote.git", "work"]);
        let fixture = Self { root };
        fixture.commit("a.txt", "a", "feat: a");
        std::fs::create_dir_all(fixture.work().join(".forge")).unwrap();
        fixture.write_land(setup, gate);
        let sweep = format!(
            "clone_dir = \"{}\"\nreport_dir = \"reports\"\ncalendar = \"*-*-* 01:00:00\"\n\
             land_checks = [\"gate\"]\n\n\
             [[step]]\nname = \"base\"\nrun = 'printf %s \"$FORGE_LAND_BASE\" > \"$FORGE_SWEEP_OUT/base.txt\" \
             && cp \"$FORGE_SWEEP_OUT/summary.md\" \"$FORGE_SWEEP_OUT/midrun.md\"'\ntimeout_s = 30\n\n\
             [[step]]\nname = \"debt\"\nrun = \"exit 9\"\ntimeout_s = 30\nblocking = false\n",
            fixture.root.join("sweep/clone").display()
        );
        fixture.commit(".forge/sweep.toml", &sweep, "chore: sweep config");
        fixture
    }
    fn work(&self) -> PathBuf {
        self.root.join("work")
    }
    fn write_land(&self, setup: &str, gate: &str) {
        let land = format!(
            "target = \"master\"\nclone_dir = \"{}\"\n\n\
             [[setup]]\nname = \"build\"\nrun = '{setup}'\ntimeout_s = 30\n\n\
             [[check]]\nname = \"gate\"\nrun = '{gate}'\npaths = ['^nothing$']\ntimeout_s = 30\n",
            self.root.join("land/clone").display()
        );
        self.commit(".forge/land.toml", &land, "chore: land config");
    }
    /// Commits `body` to `path` on master and pushes it.
    fn commit(&self, path: &str, body: &str, message: &str) {
        let work = self.work();
        std::fs::write(work.join(path), body).unwrap();
        git(&work, &["add", path]);
        git(&work, &["commit", "--quiet", "-m", message]);
        git(&work, &["push", "--quiet", "origin", "HEAD:master"]);
    }
    fn head(&self) -> String {
        git(&self.work(), &["rev-parse", "HEAD"])
    }
    fn swept(&self) -> Option<String> {
        let out = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", "refs/forge/swept"])
            .current_dir(self.root.join("sweep/clone"))
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
    /// Runs a sweep; run directories are stamped to the second, so each run
    /// waits one out first.
    fn sweep(&self) -> Output {
        std::thread::sleep(std::time::Duration::from_millis(1100));
        Command::new(env!("CARGO_BIN_EXE_forge"))
            .args(["sweep", "run"])
            .current_dir(self.work())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }
    fn latest(&self) -> String {
        std::fs::read_to_string(self.work().join("reports/latest.md")).unwrap()
    }
    /// The newest run directory's file `name`.
    fn newest(&self, name: &str) -> String {
        let mut runs: Vec<PathBuf> = std::fs::read_dir(self.work().join("reports"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.is_dir())
            .collect();
        runs.sort();
        std::fs::read_to_string(runs.last().unwrap().join(name)).unwrap()
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
fn a_green_sweep_records_its_head_and_the_next_range_starts_there() {
    // a.txt is not in the head commit: the land check sees every tracked path.
    let f = Fixture::new("true", "grep -qx a.txt \"$FORGE_LAND_CHANGED\"");
    let first_head = f.head();
    let out = f.sweep();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let latest = f.latest();
    assert!(
        latest.contains("verdict: **GREEN** (0 blocking step(s) red, 1 advisory red)"),
        "{latest}"
    );
    assert!(
        latest.contains("| debt | advisory | FAIL exit 9 |"),
        "{latest}"
    );
    assert_eq!(f.newest("summary.md"), latest);
    // The summary is rewritten after every step, so a sweep cut short says so.
    let midrun = f.newest("midrun.md");
    assert!(
        midrun.contains("verdict: **INCOMPLETE** (2 of 4 step(s) finished"),
        "{midrun}"
    );
    assert!(midrun.contains("| gate | blocking | pass |"), "{midrun}");
    let parent = git(&f.work(), &["rev-parse", "HEAD^"]);
    assert_eq!(
        f.newest("base.txt"),
        parent,
        "a first sweep covers the head commit"
    );
    assert_eq!(f.swept().as_deref(), Some(first_head.as_str()));

    f.commit("b.txt", "b", "feat: b");
    f.commit("c.txt", "c", "feat: c");
    let out = f.sweep();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(f.newest("base.txt"), first_head);
    assert!(f.latest().contains("(2 commit(s))"), "{}", f.latest());
    assert_eq!(f.swept(), Some(f.head()));
}

#[test]
fn a_red_blocking_check_keeps_the_swept_ref_so_the_range_is_checked_again() {
    let f = Fixture::new("true", "true");
    assert_eq!(f.sweep().status.code(), Some(0));
    let green = f.swept().unwrap();
    // The target's copy of the land config governs the sweep.
    f.write_land("true", "exit 4");
    let out = f.sweep();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let latest = f.latest();
    assert!(
        latest.contains("verdict: **RED** (1 blocking step(s) red, 1 advisory red)"),
        "{latest}"
    );
    assert!(latest.contains("## gate: FAIL exit 4"), "{latest}");
    assert_eq!(f.swept(), Some(green));
    assert_eq!(f.newest("base.txt"), f.swept().unwrap());
}

#[test]
fn a_failed_setup_runs_no_check_and_reports_them_not_run() {
    let f = Fixture::new("exit 3", "true");
    let out = f.sweep();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let latest = f.latest();
    assert!(
        latest.contains("| build | setup | FAIL exit 3 |"),
        "{latest}"
    );
    assert!(latest.contains("| gate | blocking | not run |"), "{latest}");
    assert!(f.swept().is_none());
}

#[test]
fn install_print_shows_the_units_and_writes_nothing() {
    let f = Fixture::new("true", "true");
    let config = f.root.join("xdg");
    let out = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["sweep", "install", "--print"])
        .current_dir(f.work())
        .env("XDG_CONFIG_HOME", &config)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let shown = text(&out);
    assert!(shown.contains("# forge-sweep-work.timer"), "{shown}");
    assert!(shown.contains("OnCalendar=*-*-* 01:00:00"), "{shown}");
    assert!(shown.contains("bound one run at 120 s"), "{shown}");
    assert!(!config.exists(), "--print wrote {}", config.display());
}
