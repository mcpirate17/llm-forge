use super::*;
use crate::process_signals::TEST_OWNER;
use std::sync::MutexGuard;

fn sh(script: &str) -> Vec<String> {
    ["/bin/sh", "-c", script].map(str::to_string).to_vec()
}

fn owner() -> MutexGuard<'static, ()> {
    TEST_OWNER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn supervised<T>(
    registry: &Path,
    memory_cap: Option<MemoryCap>,
    body: impl FnOnce(&Supervisor) -> T,
) -> T {
    let _owner = owner();
    let interrupts = Interrupts::install().unwrap();
    body(&Supervisor {
        interrupts: &interrupts,
        registry: registry.to_path_buf(),
        memory_cap,
    })
}

fn run(dir: &Path, script: &str, timeout: i64) -> Step<(CommandResult, String)> {
    supervised(&dir.join("r/live.json"), None, |supervisor| {
        supervisor.run(&sh(script), dir, timeout, &BTreeMap::new())
    })
}

/// A dead process may linger as a zombie until init reaps it.
fn process_ended(pid: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => return true,
            Ok(stat)
                if stat
                    .rsplit(')')
                    .next()
                    .unwrap_or("")
                    .trim_start()
                    .starts_with('Z') =>
            {
                return true;
            }
            Ok(_) => thread::sleep(POLL_INTERVAL),
        }
    }
    false
}

fn pid_in(path: &Path) -> String {
    fs::read_to_string(path).unwrap().trim().to_string()
}

#[test]
fn a_finished_child_reports_its_exit_and_both_streams() {
    let dir = tempfile::tempdir().unwrap();
    let (result, stdout) = run(dir.path(), "echo out; echo err >&2; exit 3", 30).unwrap();
    assert_eq!(result.returncode, Some(3));
    assert!(!result.timed_out);
    assert!(!result.succeeded());
    assert_eq!(stdout, "out\n");
    assert_eq!(result.stdout_tail, "out\n");
    assert_eq!(result.stderr_tail, "err\n");
}

#[test]
fn a_child_killed_by_a_signal_reports_the_negated_signal() {
    let dir = tempfile::tempdir().unwrap();
    let (result, _) = run(dir.path(), "kill -9 $$", 30).unwrap();
    assert_eq!(result.returncode, Some(-9));
    assert!(!result.succeeded());
}

#[test]
fn a_timeout_is_a_result_and_kills_the_whole_group() {
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let (result, _) = run(dir.path(), "sleep 300 & echo $! > pid; wait", 1).unwrap();
    assert!(result.timed_out);
    assert_eq!(result.returncode, None);
    assert!(!result.succeeded());
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(process_ended(&pid_in(&dir.path().join("pid"))));
}

#[test]
fn a_descendant_a_clean_exit_left_behind_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let script = "sleep 300 >/dev/null 2>&1 & echo $! > pid; exit 0";
    let (result, _) = run(dir.path(), script, 30).unwrap();
    assert_eq!(result.returncode, Some(0));
    assert!(process_ended(&pid_in(&dir.path().join("pid"))));
}

#[test]
fn a_descendant_holding_the_pipes_is_not_a_finished_command() {
    let dir = tempfile::tempdir().unwrap();
    let (result, _) = run(dir.path(), "sleep 300 & echo $! > pid; exit 0", 1).unwrap();
    assert!(result.timed_out);
    assert!(process_ended(&pid_in(&dir.path().join("pid"))));
}

#[test]
fn the_child_runs_in_its_own_session_with_the_declared_environment() {
    let dir = tempfile::tempdir().unwrap();
    let environment = BTreeMap::from([("DECLARED".to_string(), "bound".to_string())]);
    let script = "echo \"$DECLARED\"; cut -d' ' -f6 /proc/$$/stat; echo $$";
    let (_, stdout) = supervised(&dir.path().join("live.json"), None, |supervisor| {
        supervisor.run(&sh(script), dir.path(), 30, &environment)
    })
    .unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "bound");
    assert_eq!(lines[1], lines[2], "the child leads its session");
}

#[test]
fn the_memory_cap_binds_every_child() {
    let dir = tempfile::tempdir().unwrap();
    let cap = cap_from(Some("1")).unwrap();
    let (_, stdout) = supervised(&dir.path().join("live.json"), cap, |supervisor| {
        supervisor.run(&sh("ulimit -d"), dir.path(), 30, &BTreeMap::new())
    })
    .unwrap();
    assert_eq!(stdout.trim(), "1048576");
}

#[test]
fn an_empty_command_and_an_absent_program_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("live.json");
    let (empty, absent) = supervised(&registry, None, |supervisor| {
        let none = BTreeMap::new();
        let missing = ["/nonexistent/program".to_string()];
        (
            supervisor.run(&[], dir.path(), 30, &none),
            supervisor.run(&missing, dir.path(), 30, &none),
        )
    });
    assert_eq!(
        empty.unwrap_err(),
        Stop::Refused("refusing to run an empty command".into())
    );
    assert!(matches!(
        absent.unwrap_err(),
        Stop::Refused(message) if message.starts_with("cannot start /nonexistent/program")
    ));
    assert!(!registry.exists());
}

#[test]
fn the_registry_names_a_child_for_exactly_its_lifetime() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("iterations/live.json");
    // The record lands just after the spawn, so the child waits to see it.
    let script = format!(
        "until [ -s {path} ]; do sleep 0.01; done; cat {path}; echo $$ >&2",
        path = registry.display()
    );
    let (result, stdout) = supervised(&registry, None, |supervisor| {
        supervisor.run(&sh(&script), dir.path(), 30, &BTreeMap::new())
    })
    .unwrap();
    let held: Value = serde_json::from_str(&stdout).unwrap();
    let entry = &held["live"][0];
    assert_eq!(entry["argv0"], "/bin/sh");
    assert_eq!(entry["engine_pid"], std::process::id());
    assert_eq!(entry["pgid"].to_string(), result.stderr_tail.trim());
    assert!(!registry.exists());
    assert!(!registry.parent().unwrap().exists());
}

#[test]
fn a_run_that_cannot_record_its_pgid_refuses_and_kills_what_it_spawned() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("live.json");
    fs::write(&registry, "[]").unwrap();
    let started = Instant::now();
    let error = supervised(&registry, None, |supervisor| {
        supervisor.run(&sh("sleep 300"), dir.path(), 300, &BTreeMap::new())
    })
    .unwrap_err();
    assert!(matches!(
        error,
        Stop::Refused(message) if message.contains("is not a JSON object")
    ));
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn forgetting_one_pgid_keeps_the_others_and_the_last_removes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("iterations/live.json");
    record_live_pgid(&registry, 11, "fest").unwrap();
    record_live_pgid(&registry, 22, "pytest").unwrap();
    forget_live_pgid(&registry, 11).unwrap();
    let entries = live_entries(&registry).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["pgid"], 22);
    assert_eq!(entries[0]["argv0"], "pytest");
    forget_live_pgid(&registry, 22).unwrap();
    assert!(!registry.exists());
    assert!(!registry.parent().unwrap().exists());
    forget_live_pgid(&registry, 22).unwrap();
}

#[test]
fn removing_the_registry_leaves_a_directory_another_run_still_uses() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("iterations/live.json");
    record_live_pgid(&registry, 11, "fest").unwrap();
    fs::write(dir.path().join("iterations/receipt.json"), "{}").unwrap();
    forget_live_pgid(&registry, 11).unwrap();
    assert!(!registry.exists());
    assert!(registry.parent().unwrap().is_dir());
}

#[test]
fn a_malformed_registry_is_refused_rather_than_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("live.json");
    for body in ["not json", "[]", "{\"live\": 3}"] {
        fs::write(&registry, body).unwrap();
        let error = live_entries(&registry).unwrap_err();
        assert!(error.contains("is not a JSON object"), "{error}");
        assert!(record_live_pgid(&registry, 1, "x").is_err());
        assert_eq!(fs::read_to_string(&registry).unwrap(), body);
    }
}

#[test]
fn the_registry_sits_in_the_receipts_scratch_directory() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("pyproject.toml"),
        "[tool.conductor]\nmutation_registry = \"lanes/registry.json\"\n",
    )
    .unwrap();
    assert_eq!(
        live_pgids_path(dir.path()).unwrap(),
        dir.path()
            .join("lanes/receipts/.iterations/live_pgids.json")
    );
}

#[test]
fn a_tail_keeps_the_last_characters_not_the_last_bytes() {
    assert_eq!(tail("short"), "short");
    let long = format!("{}{}", "é".repeat(10), "ü".repeat(OUTPUT_TAIL_CHARS));
    assert_eq!(tail(&long), "ü".repeat(OUTPUT_TAIL_CHARS));
}

#[test]
fn the_cap_is_on_by_default_and_only_a_non_positive_number_disables_it() {
    let default = cap_from(None).unwrap().unwrap();
    assert!(default.limit <= default.hard);
    assert_eq!(cap_from(Some("0")).unwrap(), None);
    assert_eq!(cap_from(Some(" -1 ")).unwrap(), None);
    let one = cap_from(Some("1")).unwrap().unwrap();
    assert_eq!(one.limit, 1 << 30);
    assert_eq!(cap_from(Some("0.5")).unwrap().unwrap().limit, 1 << 29);
}

#[test]
fn a_cap_that_is_not_a_finite_number_is_refused() {
    for raw in ["lots", "", "NaN", "inf"] {
        let error = cap_from(Some(raw)).unwrap_err();
        assert!(
            error.contains("MUTATION_MEMORY_CAP_GIB is not a number"),
            "{error}"
        );
    }
}

#[test]
fn only_a_zero_exit_inside_the_bound_succeeded() {
    let result = |returncode, timed_out| CommandResult {
        returncode,
        timed_out,
        duration_seconds: 1.234_567_89,
        stdout_tail: "o".into(),
        stderr_tail: "e".into(),
    };
    assert!(result(Some(0), false).succeeded());
    assert!(!result(Some(0), true).succeeded());
    assert!(!result(Some(1), false).succeeded());
    assert!(!result(None, false).succeeded());
    assert_eq!(
        result(None, true).as_value(),
        json!({
            "returncode": null,
            "timed_out": true,
            "duration_seconds": 1.234_568,
            "stdout_tail": "o",
            "stderr_tail": "e",
        })
    );
}
