#![cfg(feature = "python-compat-tests")]
//! Command lifetime, PDEATHSIG, and orphan reaper contracts.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_support_process_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{bind_signature, buffer_text, capture, json_value, signature};
use fixture::{
    child_bin, marker_pid, pid_alive, run_command, subject, wait_dead, wait_path, OwnedProcesses,
};
use pyo3::exceptions::{PyNotImplementedError, PyOSError, PyProcessLookupError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use support::{assert_error, module, path, text, AttrPatch, Case};

fn registry(case: &Case) -> PathBuf {
    case.root().join("live_pgids.json")
}

struct RestoreCaseRoot(PathBuf);

impl Drop for RestoreCaseRoot {
    fn drop(&mut self) {
        fs::create_dir_all(&self.0).expect("restore empty fixture root before Case cleanup");
    }
}

fn field<'py, T>(result: &Bound<'py, PyAny>, name: &str) -> T
where
    T: for<'a> FromPyObject<'a, 'py>,
    for<'a> <T as FromPyObject<'a, 'py>>::Error: std::fmt::Debug,
{
    result.getattr(name).unwrap().extract().unwrap()
}

fn call_reaper<'py>(py: Python<'py>, registry: &Path, apply: bool) -> (Bound<'py, PyList>, i32) {
    let kw = PyDict::new(py);
    kw.set_item("apply", apply).unwrap();
    let answer = subject(py)
        .getattr("reap_orphaned_runs")
        .unwrap()
        .call((path(py, registry),), Some(&kw))
        .unwrap();
    (
        answer.get_item(0).unwrap().cast_into::<PyList>().unwrap(),
        answer.get_item(1).unwrap().extract().unwrap(),
    )
}

fn lines(value: &Bound<'_, PyList>) -> Vec<String> {
    value.extract().unwrap()
}

fn main_reap(py: Python<'_>, registry: &Path, apply: bool) -> i32 {
    let mut argv = vec!["reap", "--registry", registry.to_str().unwrap()];
    if apply {
        argv.push("--apply");
    }
    subject(py)
        .getattr("main")
        .unwrap()
        .call1((argv,))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn run_command_timeout_kills_the_whole_process_group() {
    let case = Case::new();
    let marker = case.root().join("grandchild.pid");
    let mut owned = OwnedProcesses::new();
    owned.track_marker(&marker);
    Python::attach(|py| {
        let argv = vec![
            "sh".into(),
            "-c".into(),
            format!("sleep 15 & echo $! > {}; sleep 15", marker.display()),
        ];
        let result = run_command(py, &argv, case.root(), &registry(&case), 2, 200).unwrap();
        assert!(field::<bool>(&result, "timed_out"));
        assert!(result.getattr("returncode").unwrap().is_none());
        let duration: f64 = field(&result, "duration_seconds");
        assert!(duration > 0.0 && duration < 15.0, "{duration}");
        let grandchild = marker_pid(&marker);
        assert!(
            wait_dead(grandchild, Duration::from_secs(10)),
            "pid {grandchild} survived the process-group timeout"
        );
    });
}

#[test]
fn run_command_reaps_descendants_a_cleanly_exiting_command_left() {
    let case = Case::new();
    let marker = case.root().join("leaked.pid");
    let mut owned = OwnedProcesses::new();
    owned.track_marker(&marker);
    Python::attach(|py| {
        let (stderr, _guard) = capture(py, "stderr");
        let argv = vec![
            "sh".into(),
            "-c".into(),
            format!(
                "sleep 300 >/dev/null 2>&1 & echo $! > {}; exit 0",
                marker.display()
            ),
        ];
        let registry = registry(&case);
        let result = run_command(py, &argv, case.root(), &registry, 30, 200).unwrap();
        assert!(!field::<bool>(&result, "timed_out"));
        assert_eq!(field::<i32>(&result, "returncode"), 0);
        let leaked = marker_pid(&marker);
        assert!(
            wait_dead(leaked, Duration::from_secs(10)),
            "pid {leaked} survived a clean command exit"
        );
        assert!(buffer_text(&stderr).contains("without reaping process group"));
        assert!(!registry.exists());
    });
}

#[test]
fn run_command_completion_is_unaffected_by_the_new_session() {
    let case = Case::new();
    let _restore = RestoreCaseRoot(case.root().to_path_buf());
    Python::attach(|py| {
        let registry = registry(&case);
        let argv = vec![
            "sh".into(),
            "-c".into(),
            "printf out; printf err >&2; exit 3".into(),
        ];
        let result = run_command(
            py,
            &argv,
            &std::env::current_dir().unwrap(),
            &registry,
            30,
            200,
        )
        .unwrap();
        assert_eq!(field::<i32>(&result, "returncode"), 3);
        assert!(!field::<bool>(&result, "timed_out"));
        assert_eq!(field::<String>(&result, "stdout_tail"), "out");
        assert_eq!(field::<String>(&result, "stderr_tail"), "err");
        let duration: f64 = field(&result, "duration_seconds");
        assert!(duration > 0.0 && duration < 30.0);
        assert!(!registry.exists());
    });
}

#[test]
fn the_caller_s_registry_is_the_one_the_run_records_its_pgid_in() {
    let mut case = Case::new();
    let host = case.mkdir("host");
    case.set_env("CONDUCTOR_HOST_ROOT", host.to_str().unwrap());
    Python::attach(|py| {
        let registry = registry(&case);
        let seen = case.root().join("seen.json");
        let argv = vec![
            "sh".into(),
            "-c".into(),
            format!("cat {} > {}", registry.display(), seen.display()),
        ];
        let result = run_command(py, &argv, case.root(), &registry, 30, 200).unwrap();
        assert_eq!(
            field::<i32>(&result, "returncode"),
            0,
            "{}",
            field::<String>(&result, "stderr_tail")
        );
        let payload: Value = serde_json::from_slice(&fs::read(&seen).unwrap()).unwrap();
        let entries = payload["live"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["argv0"], "sh");
        assert_eq!(entries[0]["engine_pid"], std::process::id());
        assert!(!registry.exists());
    });
}

fn stub(py: Python<'_>, pid: i32, surviving: bool) -> Bound<'_, PyAny> {
    let communicate = PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        let count = args.len() + kwargs.map_or(0, |value| value.len());
        if count > 1 || (count == 1 && args.len() == 1 && kwargs.is_some_and(|kw| !kw.is_empty())) {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "communicate(timeout=None)",
            ));
        }
        if let Some(kw) = kwargs {
            if !kw.is_empty() && !kw.contains("timeout")? {
                return Err(pyo3::exceptions::PyTypeError::new_err("unexpected keyword"));
            }
        }
        if surviving {
            let timeout = if let Some(kw) = kwargs {
                kw.get_item("timeout")?
                    .map(|v| v.extract::<i32>())
                    .transpose()?
                    .unwrap_or(0)
            } else if args.len() == 1 {
                args.get_item(0)?.extract::<i32>()?
            } else {
                0
            };
            let error = args
                .py()
                .import("subprocess")?
                .getattr("TimeoutExpired")?
                .call1((vec!["pretend-engine"], timeout))?;
            return Err(pyo3::PyErr::from_value(error));
        }
        Ok(("tail", ""))
    })
    .unwrap();
    let kw = PyDict::new(py);
    kw.set_item("pid", pid).unwrap();
    kw.set_item("args", vec!["pretend-engine"]).unwrap();
    kw.set_item("communicate", communicate).unwrap();
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kw))
        .unwrap()
}

#[test]
fn kill_process_group_reports_both_ends_of_the_race() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = subject(py);
        let os = subject.getattr("os").unwrap();
        let calls = Arc::new(Mutex::new(Vec::<(i32, i32)>::new()));
        let seen = Arc::clone(&calls);
        let sig = signature(py, &["pgid", "sig"], &[]);
        let signal = PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<()> {
            let bound = bind_signature(&sig, args, kw)?;
            let values = bound.getattr("arguments")?;
            seen.lock().unwrap().push((
                values.get_item("pgid")?.extract()?,
                values.get_item("sig")?.extract()?,
            ));
            Ok(())
        })
        .unwrap();
        let patch = AttrPatch::replace(&os, "killpg", signal.as_any());
        let kw = PyDict::new(py);
        kw.set_item("drain_seconds", 0).unwrap();
        let error = subject
            .getattr("_kill_process_group")
            .unwrap()
            .call((stub(py, 4242, true),), Some(&kw))
            .unwrap_err();
        assert_error(
            py,
            error,
            &subject.getattr("OrphanedProcessGroupError").unwrap(),
            "pgid 4242 still held its pipes",
        );
        assert_eq!(*calls.lock().unwrap(), vec![(4242, libc::SIGKILL)]);
        drop(patch);

        let sig = signature(py, &["pgid", "sig"], &[]);
        let gone = PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<()> {
            let bound = bind_signature(&sig, args, kw)?;
            let pgid: i32 = bound.getattr("arguments")?.get_item("pgid")?.extract()?;
            Err(PyProcessLookupError::new_err(pgid))
        })
        .unwrap();
        let _patch = AttrPatch::replace(&os, "killpg", gone.as_any());
        let result = subject
            .getattr("_kill_process_group")
            .unwrap()
            .call((stub(py, 4243, false),), Some(&kw))
            .unwrap();
        assert!(result.eq(PyTuple::new(py, ["tail", ""]).unwrap()).unwrap());
    });
}

#[test]
fn a_command_dies_with_the_process_that_spawned_it() {
    let case = Case::new();
    let registry = registry(&case);
    let finished = case.root().join("child-finished");
    let mut owned = OwnedProcesses::new();
    let engine = owned.spawn_child(
        Command::new(child_bin())
            .args([
                "engine",
                registry.to_str().unwrap(),
                finished.to_str().unwrap(),
                case.root().to_str().unwrap(),
            ])
            .current_dir(case.root()),
    );
    wait_path(&registry, Duration::from_secs(30));
    Python::attach(|py| {
        let entries = subject(py)
            .getattr("_live_pgid_entries")
            .unwrap()
            .call1((path(py, &registry),))
            .unwrap();
        assert_eq!(entries.len().unwrap(), 1);
        assert_eq!(
            entries
                .get_item(0)
                .unwrap()
                .get_item("engine_pid")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            engine
        );
        let pgid: i32 = entries
            .get_item(0)
            .unwrap()
            .get_item("pgid")
            .unwrap()
            .extract()
            .unwrap();
        owned.track_group(pgid);
        let deadline = Instant::now() + Duration::from_secs(30);
        while fs::read_to_string(format!("/proc/{pgid}/comm"))
            .unwrap_or_default()
            .trim()
            != "sleep"
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            fs::read_to_string(format!("/proc/{pgid}/comm"))
                .unwrap()
                .trim(),
            "sleep"
        );
        let killed_at = Instant::now();
        assert_eq!(unsafe { libc::kill(engine, libc::SIGKILL) }, 0);
        let _ = owned.wait_child(engine);
        assert!(
            wait_dead(pgid, Duration::from_secs(2)),
            "pid {pgid} outlived its engine by {:.2}s",
            killed_at.elapsed().as_secs_f64()
        );
    });
}

#[test]
fn a_run_that_cannot_record_its_pgid_kills_what_it_spawned() {
    let case = Case::new();
    let marker = case.root().join("spawned.pid");
    let mut owned = OwnedProcesses::new();
    owned.track_marker(&marker);
    Python::attach(|py| {
        let marker_for_callback = marker.clone();
        let refuse = PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<()> {
            wait_path(&marker_for_callback, Duration::from_secs(10));
            Err(PyOSError::new_err((
                28,
                format!("No space left on device for {args:?} {kw:?}"),
            )))
        })
        .unwrap();
        let subject = subject(py);
        let _patch = AttrPatch::replace(subject.as_any(), "record_live_pgid", refuse.as_any());
        let argv = vec![
            "sh".into(),
            "-c".into(),
            format!("echo $$ > {}; sleep 30", marker.display()),
        ];
        let error = run_command(py, &argv, case.root(), &registry(&case), 30, 10).unwrap_err();
        assert_error(
            py,
            error,
            &module(py, "builtins").getattr("OSError").unwrap(),
            "No space left",
        );
        let spawned = marker_pid(&marker);
        let deadline = Instant::now() + Duration::from_secs(10);
        while pid_alive(spawned) && Instant::now() < deadline {
            let mut status = 0;
            let waited = unsafe { libc::waitpid(spawned, &mut status, libc::WNOHANG) };
            if waited != 0 {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !pid_alive(spawned),
            "pid {spawned} survived the registry refusal"
        );
    });
}

#[test]
fn parent_death_binding_refuses_non_linux_loudly() {
    let _case = Case::new();
    Python::attach(|py| {
        for platform in ["darwin", "win32"] {
            let kw = PyDict::new(py);
            kw.set_item("platform", platform).unwrap();
            assert_error(
                py,
                subject(py)
                    .getattr("_parent_death_preexec")
                    .unwrap()
                    .call((), Some(&kw))
                    .unwrap_err(),
                &py.get_type::<PyNotImplementedError>().into_any(),
                "PR_SET_PDEATHSIG",
            );
        }
    });
}

#[test]
fn the_binding_is_actually_set_on_the_spawned_command() {
    let _case = Case::new();
    Python::attach(|py| {
        let subprocess = module(py, "subprocess");
        let kw = PyDict::new(py);
        kw.set_item("capture_output", true).unwrap();
        kw.set_item("text", true).unwrap();
        kw.set_item("timeout", 30).unwrap();
        kw.set_item("check", true).unwrap();
        kw.set_item("start_new_session", true).unwrap();
        kw.set_item(
            "preexec_fn",
            subject(py)
                .getattr("_parent_death_preexec")
                .unwrap()
                .call0()
                .unwrap(),
        )
        .unwrap();
        let result = subprocess
            .getattr("run")
            .unwrap()
            .call((vec![child_bin(), "pdeathsig"],), Some(&kw))
            .unwrap();
        assert_eq!(
            text(&result.getattr("stdout").unwrap()).trim(),
            libc::SIGKILL.to_string()
        );
    });
}

#[test]
fn a_liveness_probe_does_not_disturb_what_it_asks_about() {
    let case = Case::new();
    let mut owned = OwnedProcesses::new();
    let probe = owned.spawn_group(Command::new("sleep").arg("300").current_dir(case.root()));
    Python::attach(|py| {
        let subject = subject(py);
        assert!(subject
            .getattr("_pid_alive")
            .unwrap()
            .call1((probe,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(subject
            .getattr("_group_alive")
            .unwrap()
            .call1((probe,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let deadline = Instant::now() + Duration::from_secs(1);
        while pid_alive(probe) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            pid_alive(probe),
            "pid {probe} died while only probed with signal 0"
        );
    });
}

struct OrphanScene {
    owned: OwnedProcesses,
    registry: PathBuf,
    dead_engine: i32,
    orphan: i32,
    bystander: i32,
    in_flight: i32,
    live_engine: i32,
}

impl OrphanScene {
    fn new(py: Python<'_>, case: &Case) -> Self {
        let mut owned = OwnedProcesses::new();
        let dead_engine = owned.spawn_child(Command::new("sh").args(["-c", "exit 0"]));
        assert!(owned.wait_child(dead_engine).success());
        assert!(!pid_alive(dead_engine));
        let orphan = owned.spawn_group(Command::new("sleep").arg("300"));
        let bystander = owned.spawn_group(Command::new("sleep").arg("300"));
        let in_flight = owned.spawn_group(Command::new("sleep").arg("300"));
        let live_engine = owned.spawn_group(Command::new("sleep").arg("300"));
        let registry = registry(case);
        for (pgid, engine_pid) in [(orphan, dead_engine), (in_flight, live_engine)] {
            let kw = PyDict::new(py);
            kw.set_item("pgid", pgid).unwrap();
            kw.set_item("engine_pid", engine_pid).unwrap();
            kw.set_item("argv0", "sleep").unwrap();
            subject(py)
                .getattr("record_live_pgid")
                .unwrap()
                .call((path(py, &registry),), Some(&kw))
                .unwrap();
        }
        let entries = subject(py)
            .getattr("_live_pgid_entries")
            .unwrap()
            .call1((path(py, &registry),))
            .unwrap();
        let mut rows = json_value(&entries).as_array().unwrap().clone();
        rows.push(json!({"engine_pid":dead_engine,"pgid":dead_engine}));
        fs::write(&registry, format!("{}\n", json!({"live":rows}))).unwrap();
        Self {
            owned,
            registry,
            dead_engine,
            orphan,
            bystander,
            in_flight,
            live_engine,
        }
    }

    fn recorded_pgids(&self, py: Python<'_>) -> std::collections::BTreeSet<i32> {
        let entries = subject(py)
            .getattr("_live_pgid_entries")
            .unwrap()
            .call1((path(py, &self.registry),))
            .unwrap();
        json_value(&entries)
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["pgid"].as_i64().unwrap() as i32)
            .collect()
    }
}

#[test]
fn reap_dry_run_lists_findings_and_touches_nothing() {
    let case = Case::new();
    Python::attach(|py| {
        let scene = OrphanScene::new(py, &case);
        let (report, found) = call_reaper(py, &scene.registry, false);
        let rows = lines(&report);
        assert_eq!(found, 1);
        assert!(rows.iter().any(
            |line| line.contains(&format!("pgid {} (sleep)", scene.orphan))
                && line.contains("would SIGKILL")
        ));
        assert!(rows
            .iter()
            .all(|line| !line.contains(&format!("pgid {}", scene.in_flight))
                || line.contains("in flight")));
        assert!(rows
            .iter()
            .any(|line| line.contains("(?)") && line.contains("stale")));
        assert!(!rows
            .iter()
            .any(|line| line.contains(&format!("pgid {}", scene.bystander))));
        assert!(pid_alive(scene.orphan));
        assert!(pid_alive(scene.bystander));
        assert!(pid_alive(scene.in_flight));
        assert_eq!(main_reap(py, &scene.registry, false), 1);
        assert!(pid_alive(scene.orphan));
        assert!(pid_alive(scene.live_engine));
    });
}

#[test]
fn reap_finds_the_orphan_whose_group_leader_already_exited() {
    let case = Case::new();
    let registry = registry(&case);
    let marker = case.root().join("grandchild.pid");
    let mut owned = OwnedProcesses::new();
    let pgid =
        owned.spawn_group(Command::new(child_bin()).args(["leaderless", marker.to_str().unwrap()]));
    assert!(owned.wait_child(pgid).success());
    let grandchild = marker_pid(&marker);
    Python::attach(|py| {
        assert!(!pid_alive(pgid));
        assert!(pid_alive(grandchild));
        assert_eq!(unsafe { libc::getpgid(grandchild) }, pgid);
        let kw = PyDict::new(py);
        kw.set_item("pgid", pgid).unwrap();
        kw.set_item("engine_pid", pgid).unwrap();
        kw.set_item("argv0", "fest").unwrap();
        subject(py)
            .getattr("record_live_pgid")
            .unwrap()
            .call((path(py, &registry),), Some(&kw))
            .unwrap();
        let (report, found) = call_reaper(py, &registry, false);
        assert_eq!(found, 1, "{:?}", lines(&report));
        assert!(lines(&report)
            .iter()
            .any(|line| line.contains("would SIGKILL")));
        assert!(!lines(&report).iter().any(|line| line.contains("stale")));
        assert!(pid_alive(grandchild));
        let (_, found) = call_reaper(py, &registry, true);
        assert_eq!(found, 1);
        assert!(
            wait_dead(grandchild, Duration::from_secs(10)),
            "--apply did not reach leaderless group {pgid}"
        );
    });
}

#[test]
fn reap_apply_kills_only_the_recorded_orphan() {
    let case = Case::new();
    Python::attach(|py| {
        let mut scene = OrphanScene::new(py, &case);
        let (_, found) = call_reaper(py, &scene.registry, true);
        assert_eq!(found, 1);
        assert_eq!(
            scene.owned.wait_child(scene.orphan).signal(),
            Some(libc::SIGKILL)
        );
        assert!(pid_alive(scene.bystander));
        assert!(pid_alive(scene.in_flight));
        assert_eq!(
            scene.recorded_pgids(py),
            [scene.in_flight, scene.dead_engine].into()
        );
        assert_eq!(main_reap(py, &scene.registry, true), 0);
    });
}
