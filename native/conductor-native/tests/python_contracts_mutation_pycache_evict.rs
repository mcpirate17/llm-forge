#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the child bytecode-eviction plugin.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::{PyRuntimeError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyModule, PyTuple};
use std::ffi::OsString;
use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use support::{assert_error, module, path, text, AttrPatch, Case};

const SCRATCH_ENV: &str = "CONDUCTOR_PYCACHE_EVICT_SCRATCH";
const SOURCES_ENV: &str = "CONDUCTOR_PYCACHE_EVICT_SOURCES";

fn eviction_case() -> Case {
    let mut case = Case::new();
    case.remove_env(SCRATCH_ENV);
    case.remove_env(SOURCES_ENV);
    case
}

fn scratch_root(root: &Path) -> PathBuf {
    Python::attach(|py| {
        module(py, "conductor.bytecode_isolation")
            .getattr("scratch_root_for")
            .unwrap()
            .call1((path(py, root),))
            .unwrap()
            .extract()
            .unwrap()
    })
}

fn cache_paths(source: &Path, scratch: &Path) -> Vec<PathBuf> {
    Python::attach(|py| {
        module(py, "conductor.bytecode_isolation")
            .getattr("cache_paths_for")
            .unwrap()
            .call1((path(py, source), path(py, &scratch.join("pycache"))))
            .unwrap()
            .extract()
            .unwrap()
    })
}

fn marker_name() -> String {
    Python::attach(|py| {
        module(py, "conductor.mutation_pycache_evict")
            .getattr("_RUN_MARKER")
            .unwrap()
            .extract()
            .unwrap()
    })
}

fn evict_now() -> PyResult<Vec<PathBuf>> {
    Python::attach(|py| {
        module(py, "conductor.mutation_pycache_evict")
            .getattr("evict_now")
            .unwrap()
            .call0()?
            .extract()
    })
}

fn broken_cache_paths<'py>(py: Python<'py>, plugin: &Bound<'py, PyModule>) -> AttrPatch {
    let broken = PyCFunction::new_closure(
        py,
        None,
        None,
        |_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
            Err(PyTypeError::new_err("mutated away"))
        },
    )
    .unwrap();
    AttrPatch::replace(plugin.as_any(), "_cache_paths", broken.as_any())
}

fn fail_closed(scratch: &Path) -> PyResult<()> {
    Python::attach(|py| {
        module(py, "conductor.mutation_pycache_evict")
            .getattr("_fail_closed_delete")
            .unwrap()
            .call1((scratch.to_str().unwrap(),))?;
        Ok(())
    })
}

fn assert_refusal(scratch: &Path, reason: &str) {
    Python::attach(|py| {
        assert_error(
            py,
            fail_closed(scratch).unwrap_err(),
            py.get_type::<PyRuntimeError>().as_any(),
            reason,
        );
    });
}

fn python_executable() -> OsString {
    std::env::var_os("PYO3_PYTHON")
        .or_else(|| std::env::var_os("PYTHON"))
        .unwrap_or_else(|| OsString::from("python3"))
}

fn bounded_output(mut command: Command, seconds: u64) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "Python child timed out: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn child(
    root: &Path,
    scratch: &Path,
    source: Option<&Path>,
    args: &[&str],
    pytest_options: Option<&str>,
    seconds: u64,
) -> Output {
    let source_tree = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
    let mut python_paths = vec![source_tree];
    if let Some(inherited) = std::env::var_os("PYTHONPATH") {
        python_paths.extend(std::env::split_paths(&inherited));
    }
    let mut command = Command::new(python_executable());
    command
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("PYTHONPATH", std::env::join_paths(python_paths).unwrap())
        .env("PYTHONPYCACHEPREFIX", scratch.join("pycache"));
    if source.is_some() {
        command.env(SCRATCH_ENV, scratch);
    }
    if let Some(source) = source {
        command.env(SOURCES_ENV, source);
    }
    if let Some(options) = pytest_options {
        command.env("PYTEST_ADDOPTS", options);
    }
    bounded_output(command, seconds)
}

#[test]
fn evict_now_deletes_every_tag_of_the_named_sources() {
    let mut case = eviction_case();
    let source = case.write("m.py", "x = 1\n");
    let scratch = scratch_root(case.root());
    let caches = cache_paths(&source, &scratch);
    assert_eq!(caches.len(), 3);
    for cache in &caches {
        fs::create_dir_all(cache.parent().unwrap()).unwrap();
        fs::write(cache, b"stale").unwrap();
    }
    case.set_env(SCRATCH_ENV, scratch.to_str().unwrap());
    case.set_env(SOURCES_ENV, source.to_str().unwrap());
    assert_eq!(evict_now().unwrap().len(), caches.len());
    assert!(caches.iter().all(|cache| !cache.exists()));
}

#[test]
fn evict_now_without_both_engine_variables_does_nothing() {
    let mut case = eviction_case();
    assert!(evict_now().unwrap().is_empty());
    case.set_env(SOURCES_ENV, case.root().join("m.py").to_str().unwrap());
    assert!(evict_now().unwrap().is_empty());
}

#[test]
fn a_broken_eviction_fails_closed_on_the_whole_prefix() {
    let mut case = eviction_case();
    let source = case.write("m.py", "x = 1\n");
    let scratch = scratch_root(case.root());
    let stale = cache_paths(&source, &scratch).remove(0);
    fs::create_dir_all(stale.parent().unwrap()).unwrap();
    fs::write(&stale, b"stale").unwrap();
    fs::write(scratch.join(marker_name()), "").unwrap();
    case.set_env(SCRATCH_ENV, scratch.to_str().unwrap());
    case.set_env(SOURCES_ENV, source.to_str().unwrap());
    Python::attach(|py| {
        let plugin = module(py, "conductor.mutation_pycache_evict");
        let _patch = broken_cache_paths(py, &plugin);
        assert!(evict_now().unwrap().is_empty());
    });
    assert!(!scratch.exists());
}

#[test]
fn the_fail_closed_deletion_requires_the_run_marker() {
    let mut case = eviction_case();
    let scratch = scratch_root(case.root());
    fs::create_dir_all(scratch.join("pycache")).unwrap();
    case.set_env(SCRATCH_ENV, scratch.to_str().unwrap());
    case.set_env(SOURCES_ENV, case.root().join("m.py").to_str().unwrap());
    Python::attach(|py| {
        let plugin = module(py, "conductor.mutation_pycache_evict");
        let _patch = broken_cache_paths(py, &plugin);
        assert_error(
            py,
            evict_now().unwrap_err(),
            py.get_type::<PyRuntimeError>().as_any(),
            "is absent",
        );
    });
    assert!(scratch.join("pycache").is_dir());
}

#[test]
fn the_fail_closed_deletion_requires_a_pycache_tree() {
    let mut case = eviction_case();
    let scratch = case.mkdir("marked-empty");
    fs::write(scratch.join(marker_name()), "").unwrap();
    case.set_env(SCRATCH_ENV, scratch.to_str().unwrap());
    case.set_env(SOURCES_ENV, case.root().join("m.py").to_str().unwrap());
    Python::attach(|py| {
        let plugin = module(py, "conductor.mutation_pycache_evict");
        let _patch = broken_cache_paths(py, &plugin);
        assert_error(
            py,
            evict_now().unwrap_err(),
            py.get_type::<PyRuntimeError>().as_any(),
            "pycache does not exist",
        );
    });
    assert!(scratch.is_dir());
}

#[test]
fn the_fail_closed_deletion_refuses_the_four_forbidden_places() {
    let mut case = eviction_case();
    let forbidden = case.mkdir("forbidden/pycache").parent().unwrap().to_owned();
    fs::write(forbidden.join(marker_name()), "").unwrap();
    let repo = case.mkdir("repo/work").parent().unwrap().to_owned();
    fs::create_dir(repo.join(".git")).unwrap();
    fs::create_dir(repo.join("pycache")).unwrap();
    fs::write(repo.join(marker_name()), "").unwrap();
    let home = case.mkdir("home/pycache").parent().unwrap().to_owned();
    fs::write(home.join(marker_name()), "").unwrap();
    case.set_env("HOME", home.to_str().unwrap());
    let cwd = case.chdir("repo/work");

    for (place, reason) in [
        (Path::new("/"), "filesystem root"),
        (home.as_path(), "home directory"),
        (repo.as_path(), "repository root"),
        (case.root(), "parent of the working directory"),
    ] {
        assert_refusal(place, reason);
    }
    assert!(home.is_dir() && repo.is_dir() && repo.join("work").is_dir());
    assert!(repo.join("pycache").is_dir() && repo.join(marker_name()).is_file());
    fail_closed(&forbidden).unwrap();
    assert!(!forbidden.exists());
    drop(cwd);
}

#[test]
fn the_fail_closed_deletion_refuses_the_working_directory_itself() {
    let case = eviction_case();
    let scratch = case.mkdir("here/pycache").parent().unwrap().to_owned();
    fs::write(scratch.join(marker_name()), "").unwrap();
    let cwd = case.chdir("here");
    assert_refusal(&scratch, "is the working directory itself");
    assert!(scratch.is_dir() && scratch.join("pycache").is_dir());
    drop(cwd);
}

#[test]
fn the_deletion_marker_literal_matches_the_one_the_launcher_writes() {
    let _case = eviction_case();
    Python::attach(|py| {
        let plugin = module(py, "conductor.mutation_pycache_evict");
        let launcher = module(py, "conductor.bytecode_isolation");
        assert_eq!(
            text(&plugin.getattr("_RUN_MARKER").unwrap()),
            text(&launcher.getattr("RUN_MARKER_NAME").unwrap())
        );
    });
}

#[test]
fn the_plugin_imports_nothing_of_the_module_under_mutation() {
    let case = eviction_case();
    let scratch = scratch_root(case.root());
    let output = child(
        case.root(),
        &scratch,
        None,
        &["-c", "import conductor.mutation_pycache_evict, sys; print('conductor.bytecode_isolation' in sys.modules)"],
        None,
        120,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "False");
}

#[test]
fn the_plugin_evicts_at_pytest_startup_before_the_imports() {
    let case = eviction_case();
    let source = case.write("m.py", "def f(): return 1\n");
    let original = fs::metadata(&source).unwrap();
    let scratch = scratch_root(case.root());
    // This is a synthetic pytest host input, not the migrated assertion suite.
    // Rust checks both the stale failure and plugin-enabled success below.
    case.write(
        "test_m.py",
        "import m\n\n\ndef test_mutant():\n    assert m.f() == 2\n",
    );
    let warm = child(
        case.root(),
        &scratch,
        None,
        &["-c", "import m; print(m.f())"],
        None,
        120,
    );
    assert!(
        warm.status.success(),
        "{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    assert_eq!(String::from_utf8(warm.stdout).unwrap().trim(), "1");
    assert!(cache_paths(&source, &scratch)[0].is_file());

    fs::write(&source, "def f(): return 2\n").unwrap();
    File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(
            FileTimes::new()
                .set_accessed(original.accessed().unwrap())
                .set_modified(original.modified().unwrap()),
        )
        .unwrap();
    let pytest_args = ["-m", "pytest", "-q", "test_m.py"];
    let stale = child(case.root(), &scratch, None, &pytest_args, Some("-q"), 300);
    assert!(
        !stale.status.success(),
        "{}",
        String::from_utf8_lossy(&stale.stderr)
    );
    let stale_stdout = String::from_utf8(stale.stdout).unwrap();
    assert!(stale_stdout.contains("assert 1 == 2"), "{stale_stdout}");

    let plugin = Python::attach(|py| {
        text(
            &module(py, "conductor.mutation_pycache_evict")
                .getattr("PLUGIN_NAME")
                .unwrap(),
        )
    });
    let options = format!("-q -p {plugin}");

    let fresh = child(
        case.root(),
        &scratch,
        Some(&source),
        &pytest_args,
        Some(&options),
        300,
    );
    assert!(
        fresh.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&fresh.stdout),
        String::from_utf8_lossy(&fresh.stderr)
    );
}
