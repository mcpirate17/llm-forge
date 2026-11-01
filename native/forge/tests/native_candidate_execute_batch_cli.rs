#![cfg(target_os = "linux")]
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::{value, Case};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn batch(program: &Path) -> Case {
    let mut case = Case::new(program);
    case.grants["resources"] = json!({"cpu_millis":1000,"memory_mib":128,"processes":16});
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":256,"processes":32});
    case.staged_policy(|text| format!("{text}\n[checks.beta]\nkind='command'\nprofiles=['fast']\nclasses=['rust']\ncommand=['worker','echo','beta']\nversion_command=['worker','echo','beta-v1']\ntimeout_seconds=2\nmemory_mb=128\n"));
    case
}
fn command(case: &Case, checks: &[&str]) -> Command {
    case.command_for("execute-batch", "index", checks)
}
fn ids(report: &Value) -> Vec<&str> {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["check_id"].as_str().unwrap())
        .collect()
}
fn no_execution(report: &Value) {
    assert_eq!(report["executed"], false);
    for check in report["checks"].as_array().unwrap() {
        assert!(check["result"].is_null());
        for stage in check["stages"].as_array().unwrap() {
            assert!(stage["report"].is_null());
        }
    }
}
fn bounded(case: &Case, checks: &[&str]) -> std::process::Output {
    forge::process_probe::run(
        &mut command(case, checks),
        Duration::from_secs(8),
        1024 * 1024,
    )
    .unwrap()
}

#[test]
fn default_dry_run_reports_unexecuted_builtin_instead_of_complete_success() {
    let case = batch(Path::new("/bin/true"));
    case.staged_policy(|text| {
        text.replace(
            "[checks.alpha]\nkind='builtin'",
            "[checks.alpha]\nkind='builtin'\nalways=true",
        )
    });
    let before = case.source_state();
    let report = value(bounded(&case, &[]), 1);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["execution_scope"], "selected_regular_checks");
    assert_eq!(report["review_complete"], false);
    assert_eq!(report["success"], false);
    assert_eq!(ids(&report), ["zeta", "alpha", "beta"]);
    let unexecuted = report["unexecuted"].as_array().unwrap();
    assert_eq!(unexecuted.len(), 1);
    assert_eq!(unexecuted[0]["check_id"], "alpha");
    assert_eq!(unexecuted[0]["kind"], "builtin");
    assert!(!unexecuted[0]["reason"].as_str().unwrap().is_empty());
    no_execution(&report);
    case.assert_preserved(&before);
}

#[test]
fn explicit_selection_keeps_policy_order_and_staged_policy_with_empty_path() {
    let case = batch(Path::new("/bin/true"));
    let before = case.source_state();
    fs::write(
        case.fixture.root.join("repo/policies/main.toml"),
        "invalid dirty policy",
    )
    .unwrap();
    let report = value(bounded(&case, &["beta", "zeta"]), 0);
    assert_eq!(ids(&report), ["zeta", "beta"]);
    assert_eq!(report["unexecuted"], json!([]));
    assert_eq!(report["success"], true);
    assert_eq!(report["budget"]["admitted_parallelism"], 2);
    no_execution(&report);
    case.assert_preserved(&before);
}

#[test]
fn aggregate_cpu_memory_process_worker_and_policy_caps_limit_admission() {
    for (field, limit) in [
        ("max_workers", 1),
        ("cpu_millis", 1000),
        ("memory_mib", 128),
        ("processes", 16),
    ] {
        let mut case = batch(Path::new("/bin/true"));
        case.grants["batch"][field] = json!(limit);
        let before = case.source_state();
        let report = value(bounded(&case, &["zeta", "beta"]), 0);
        assert_eq!(report["budget"]["admitted_parallelism"], 1, "{field}");
        no_execution(&report);
        case.assert_preserved(&before);
    }
    let case = batch(Path::new("/bin/true"));
    case.staged_policy(|text| text.replace("max_workers=2", "max_workers=1"));
    let before = case.source_state();
    let report = value(bounded(&case, &["zeta", "beta"]), 0);
    assert_eq!(report["budget"]["admitted_parallelism"], 1);
    case.assert_preserved(&before);
}

#[test]
fn missing_underfit_unsafe_budget_and_duplicate_unknown_inactive_ids_refuse() {
    for variant in 0..5 {
        let mut case = batch(Path::new("/bin/true"));
        match variant {
            0 => {
                case.grants.as_object_mut().unwrap().remove("batch");
            }
            1 => case.grants["batch"]["memory_mib"] = json!(127),
            2 => case.grants["batch"]["processes"] = json!(15),
            3 => case.grants["batch"]["cpu_millis"] = json!(999),
            _ => case.grants["batch"]["max_workers"] = json!(0),
        }
        let before = case.source_state();
        let output = bounded(&case, &["zeta"]);
        assert!(!output.status.success(), "budget variant {variant}");
        case.assert_preserved(&before);
    }
    let case = batch(Path::new("/bin/true"));
    let before = case.source_state();
    for selected in [&["zeta", "zeta"][..], &["unknown"][..], &["full_only"][..]] {
        assert!(!bounded(&case, selected).status.success());
        case.assert_preserved(&before);
    }
}

#[test]
fn filtered_commands_skip_without_launch_even_when_execution_requested() {
    let case = batch(Path::new("/bin/true"));
    case.staged_policy(|text| text.replace("classes=['rust']", "classes=['cuda']"));
    let before = case.source_state();
    let mut cmd = command(&case, &[]);
    cmd.arg("--execute");
    let report = value(
        forge::process_probe::run(&mut cmd, Duration::from_secs(8), 1024 * 1024).unwrap(),
        0,
    );
    assert_eq!(report["success"], true);
    assert_eq!(ids(&report), ["zeta", "alpha", "beta"]);
    assert_eq!(report["unexecuted"], json!([]));
    for row in report["checks"].as_array().unwrap() {
        assert_eq!(row["skipped"], true);
        assert!(row["result"].is_null());
        assert_eq!(row["stages"], json!([]));
    }
    case.assert_preserved(&before);
}

/// Owns only a fresh delegated subtree. Never enables borrowed-parent controllers.
struct Delegation {
    path: PathBuf,
}
impl Delegation {
    fn new() -> Self {
        let parent = PathBuf::from(
            std::env::var_os("FORGE_TEST_CGROUP_PARENT")
                .expect("explicit cgroup delegation required"),
        )
        .canonicalize()
        .unwrap();
        let path = parent.join(forge::resource_id("forge-batch-test").unwrap());
        fs::create_dir(&path).unwrap();
        let owned = Self { path };
        fs::write(
            owned.path.join("cgroup.subtree_control"),
            "+cpu +memory +pids",
        )
        .unwrap();
        owned
    }
    fn attach(&self, case: &mut Case) {
        case.grants["cgroup_parent"] = json!(self.path);
    }
    fn groups(&self) -> Vec<PathBuf> {
        let groups = owned_dirs(&self.path, 4096);
        for path in &groups {
            assert!(path
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("forge-native-"));
        }
        groups
    }
    fn live(&self) -> (usize, BTreeSet<u32>) {
        let mut pids = BTreeSet::new();
        let mut populated = 0;
        for group in self.groups() {
            if let Some(raw) = read_small(&group.join("cgroup.procs"), 8192) {
                populated += usize::from(!raw.trim().is_empty());
                for pid in raw.lines() {
                    pids.insert(pid.parse().unwrap());
                }
            }
        }
        (populated, pids)
    }
    fn assert_clean(&self) {
        assert!(self.groups().is_empty(), "owned cgroups remain");
    }
}
impl Drop for Delegation {
    fn drop(&mut self) {
        for group in self.groups() {
            if let Err(error) = fs::write(group.join("cgroup.kill"), "1") {
                eprintln!("owned cgroup kill: {error}");
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match fs::remove_dir(&group) {
                    Ok(()) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
        if let Err(error) = fs::remove_dir(&self.path) {
            eprintln!("owned delegated-parent cleanup: {error}");
        }
    }
}
fn read_small(path: &Path, maximum: u64) -> Option<String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("read owned fixture: {error}"),
    };
    let mut raw = String::new();
    file.take(maximum + 1).read_to_string(&mut raw).unwrap();
    assert!(raw.len() as u64 <= maximum);
    Some(raw)
}
fn owned_dirs(path: &Path, maximum: usize) -> Vec<PathBuf> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!("read owned directory: {error}"),
    };
    let mut result = Vec::new();
    for (index, entry) in entries.enumerate() {
        assert!(index < maximum, "owned directory entry limit");
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("owned directory entry: {error}"),
        };
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => result.push(entry.path()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("owned entry type: {error}"),
        }
    }
    result
}
fn ready_pids(case: &Case) -> BTreeSet<u32> {
    let mut pids = BTreeSet::new();
    for stage in owned_dirs(&case.fixture.root.join("scratch"), 64) {
        for child in owned_dirs(&stage, 16) {
            if let Some(raw) = read_small(&child.join("ready.pid"), 32) {
                if let Ok(pid) = raw.parse() {
                    pids.insert(pid);
                }
            }
        }
    }
    pids
}
struct Running {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}
impl Running {
    fn start(case: &Case, checks: &[&str]) -> Self {
        let stdout = case.fixture.root.join("batch.stdout");
        let stderr = case.fixture.root.join("batch.stderr");
        let child = command(case, checks)
            .arg("--execute")
            .stdin(Stdio::null())
            .stdout(fs::File::create(&stdout).unwrap())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        Self {
            child,
            stdout,
            stderr,
        }
    }
    fn finish(&mut self, deadline: Instant, expected: i32) -> Value {
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(
                    status.code(),
                    Some(expected),
                    "{}",
                    fs::read_to_string(&self.stderr).unwrap()
                );
                return serde_json::from_slice(&fs::read(&self.stdout).unwrap()).unwrap();
            }
            assert!(Instant::now() < deadline, "batch watchdog expired");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        // SAFETY: the unreaped owned Child pins its PID until wait.
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Err(error) = self.child.kill() {
            eprintln!("batch CLI emergency kill: {error}");
        }
        if let Err(error) = self.child.wait() {
            eprintln!("batch CLI emergency reap: {error}");
        }
    }
}
fn sleeping(case: &Case, milliseconds: u32) {
    case.staged_policy(|text| {
        text.replace("timeout_seconds=2", "timeout_seconds=10")
            .replace(
                "command=['worker','echo','literal argument','{files}']",
                &format!("command=['worker','pid-sleep','{milliseconds}','ready.pid']"),
            )
            .replace(
                "command=['worker','echo','beta']",
                &format!("command=['worker','pid-sleep','{milliseconds}','ready.pid']"),
            )
    });
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT with delegated cpu/memory/pids controllers"]
fn independent_checks_overlap_only_within_admitted_owned_cgroup_budget() {
    let mut case = batch(support::task_worker());
    let delegation = Delegation::new();
    delegation.attach(&mut case);
    sleeping(&case, 2500);
    let before = case.source_state();
    let mut running = Running::start(&case, &["beta", "zeta"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed_two = false;
    loop {
        let (populated, live) = delegation.live();
        let ready = ready_pids(&case);
        assert!(
            populated <= 2,
            "more populated groups than admitted parallelism"
        );
        if ready.intersection(&live).count() == 2 {
            observed_two = true;
        }
        if running.child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "parallel batch watchdog expired");
        std::thread::sleep(Duration::from_millis(10));
    }
    let report = running.finish(deadline, 0);
    assert!(
        observed_two,
        "two independently ready command workers never overlapped"
    );
    assert_eq!(report["budget"]["admitted_parallelism"], 2);
    assert_eq!(ids(&report), ["zeta", "beta"]);
    for row in report["checks"].as_array().unwrap() {
        assert_eq!(row["result"]["status"], "passed");
    }
    delegation.assert_clean();
    case.assert_preserved(&before);
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT with delegated cpu/memory/pids controllers"]
fn failed_version_blocks_only_its_check_and_other_results_remain_visible() {
    let mut case = batch(support::task_worker());
    let delegation = Delegation::new();
    delegation.attach(&mut case);
    case.staged_policy(|text| {
        text.replace(
            "version_command=['worker','echo','fixture-v1']",
            "version_command=['worker','exit','7']",
        )
        .replace(
            "command=['worker','echo','literal argument','{files}']",
            "command=['worker','pid-sleep','20000','ready.pid']",
        )
        .replace(
            "command=['worker','echo','beta']",
            "command=['worker','exit','3']",
        )
    });
    let before = case.source_state();
    let mut running = Running::start(&case, &["zeta", "beta"]);
    let report = running.finish(Instant::now() + Duration::from_secs(8), 1);
    assert_eq!(ids(&report), ["zeta", "beta"]);
    assert_eq!(report["success"], false);
    let rows = report["checks"].as_array().unwrap();
    assert_eq!(
        rows[0]["result"]["findings"][0]["rule_id"],
        "required-analyzer-unavailable"
    );
    assert_eq!(rows[0]["stages"].as_array().unwrap().len(), 1);
    assert_eq!(
        rows[1]["result"]["findings"][0]["rule_id"],
        "analyzer-finding"
    );
    assert_eq!(rows[1]["result"]["exit_code"], 3);
    delegation.assert_clean();
    case.assert_preserved(&before);
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT with delegated cpu/memory/pids controllers"]
fn sigterm_waits_for_all_owned_children_and_never_reports_batch_success() {
    let mut case = batch(support::task_worker());
    let delegation = Delegation::new();
    delegation.attach(&mut case);
    sleeping(&case, 20000);
    let before = case.source_state();
    let mut running = Running::start(&case, &["zeta", "beta"]);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let ready = ready_pids(&case);
        let (populated, live) = delegation.live();
        assert!(populated <= 2, "cancellation exceeded admitted parallelism");
        if ready.intersection(&live).count() == 2 {
            break;
        }
        assert!(
            running.child.try_wait().unwrap().is_none(),
            "batch ended before both workers became ready"
        );
        assert!(
            Instant::now() < deadline,
            "batch cancellation readiness watchdog expired"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: this owned child is alive and unreaped, preventing PID reuse.
    assert_eq!(
        unsafe { libc::kill(running.child.id() as i32, libc::SIGTERM) },
        0
    );
    let report = running.finish(deadline, 1);
    assert_eq!(report["success"], false);
    assert_eq!(ids(&report), ["zeta", "beta"]);
    for row in report["checks"].as_array().unwrap() {
        assert!(row["result"]["status"] == "failed" || row["error"].is_string());
        for stage in row["stages"].as_array().unwrap() {
            if !stage["report"].is_null() {
                assert_eq!(stage["report"]["cleanup_complete"], true);
            }
        }
    }
    delegation.assert_clean();
    case.assert_preserved(&before);
}
