#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for run-private Python bytecode caches.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, FileTimes};
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use support::{assert_error, module, path, text, Case};

const MODULE_ONE: &str = "def f(): return 1\n";
const MODULE_TWO: &str = "def f(): return 2\n";
const MODULE_THREE: &str = "def f(): return 3\n";
const MODULE_FOUR: &str = "def f(): return 4\n";
const CHILD_F: &str = "import m; print(m.f())";
const CHILD_FG: &str = "import m, n; print(m.f(), n.g())";

fn python_executable() -> OsString {
    std::env::var_os("PYO3_PYTHON")
        .or_else(|| std::env::var_os("PYTHON"))
        .unwrap_or_else(|| OsString::from("python3"))
}

fn plain_env() -> HashMap<String, String> {
    std::env::vars()
        .filter(|(key, _)| key != "PYTHONDONTWRITEBYTECODE" && key != "PYTHONPYCACHEPREFIX")
        .collect()
}

fn child(env: &HashMap<String, String>, cwd: &Path, program: &str) -> String {
    let mut command = Command::new(python_executable());
    command
        .args(["-c", program])
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = command.spawn().expect("launch Python fixture child");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if process.try_wait().expect("poll Python child").is_some() {
            let output = process
                .wait_with_output()
                .expect("read Python child output");
            assert_child_success(&output);
            return String::from_utf8(output.stdout)
                .expect("UTF-8 child stdout")
                .trim()
                .to_owned();
        }
        if Instant::now() >= deadline {
            process.kill().expect("stop timed-out Python fixture child");
            let output = process.wait_with_output().expect("read timed-out child");
            panic!(
                "Python fixture child timed out: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_child_success(output: &Output) {
    assert!(
        output.status.success(),
        "Python fixture child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn isolated_env(
    base: &HashMap<String, String>,
    scratch: &Path,
    mutated: &[&Path],
) -> HashMap<String, String> {
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        let sources: Vec<_> = mutated.iter().map(|source| path(py, source)).collect();
        kwargs.set_item("mutated_paths", sources).unwrap();
        module(py, "conductor.bytecode_isolation")
            .getattr("isolated_python_env")
            .unwrap()
            .call((base, path(py, scratch)), Some(&kwargs))
            .unwrap()
            .extract()
            .unwrap()
    })
}

fn cache_paths(source: &Path, prefix: &Path) -> Vec<PathBuf> {
    Python::attach(|py| {
        module(py, "conductor.bytecode_isolation")
            .getattr("cache_paths_for")
            .unwrap()
            .call1((path(py, source), path(py, prefix)))
            .unwrap()
            .extract()
            .unwrap()
    })
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

fn pinned_mtime_write(source: &Path, content: &str, original: &fs::Metadata) {
    assert_eq!(
        content.len(),
        original.len() as usize,
        "fixture byte count must stay fixed"
    );
    fs::write(source, content).unwrap();
    let times = FileTimes::new()
        .set_accessed(original.accessed().unwrap())
        .set_modified(original.modified().unwrap());
    File::options()
        .write(true)
        .open(source)
        .unwrap()
        .set_times(times)
        .unwrap();
}

fn pyc_count(root: &Path) -> usize {
    fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .map(|path| {
            if path.is_dir() {
                pyc_count(&path)
            } else {
                usize::from(path.extension().is_some_and(|extension| extension == "pyc"))
            }
        })
        .sum()
}

#[test]
fn same_second_edit_runs_stale_code_until_env_is_isolated() {
    let case = Case::new();
    let source = case.write("m.py", MODULE_ONE);
    let original = fs::metadata(&source).unwrap();
    let plain = plain_env();
    assert_eq!(child(&plain, case.root(), CHILD_F), "1");
    let caches: Vec<_> = fs::read_dir(case.root().join("__pycache__"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|file| {
            file.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("m.")
        })
        .collect();
    assert!(
        !caches.is_empty(),
        "plain child must write beside-source cache"
    );
    pinned_mtime_write(&source, MODULE_TWO, &original);
    assert_eq!(child(&plain, case.root(), CHILD_F), "1");
    let isolated = isolated_env(&plain, &case.root().join("scratch"), &[]);
    assert_eq!(child(&isolated, case.root(), CHILD_F), "2");
}

#[test]
fn env_carries_base_plus_run_private_prefix() {
    let case = Case::new();
    let base = HashMap::from([
        ("KEEP".to_owned(), "yes".to_owned()),
        ("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned()),
    ]);
    let env = isolated_env(&base, &case.root().join("scratch"), &[]);
    assert_eq!(env.get("KEEP").map(String::as_str), Some("yes"));
    assert!(!env.contains_key("PYTHONDONTWRITEBYTECODE"));
    assert_eq!(
        env["PYTHONPYCACHEPREFIX"],
        case.root().join("scratch/pycache").display().to_string()
    );
    assert_eq!(base["PYTHONDONTWRITEBYTECODE"], "1");
}

#[test]
fn scratch_is_marked_the_moment_it_is_created() {
    let case = Case::new();
    isolated_env(&HashMap::new(), &case.root().join("scratch"), &[]);
    Python::attach(|py| {
        let marker_name = text(
            &module(py, "conductor.bytecode_isolation")
                .getattr("RUN_MARKER_NAME")
                .unwrap(),
        );
        let marker = case.root().join("scratch").join(marker_name);
        assert!(marker.is_file());
        assert_eq!(fs::read(marker).unwrap(), b"");
    });
}

#[test]
fn each_scratch_names_its_own_prefix() {
    let case = Case::new();
    let one = isolated_env(&HashMap::new(), &case.root().join("one"), &[]);
    let two = isolated_env(&HashMap::new(), &case.root().join("two"), &[]);
    assert_ne!(one["PYTHONPYCACHEPREFIX"], two["PYTHONPYCACHEPREFIX"]);
}

#[test]
fn unusable_scratch_fails_loud() {
    let case = Case::new();
    let blocker = case.write("not-a-directory", "{}");
    Python::attach(|py| {
        let error = module(py, "conductor.bytecode_isolation")
            .getattr("isolated_python_env")
            .unwrap()
            .call1((
                HashMap::<String, String>::new(),
                path(py, &blocker.join("pycache")),
            ))
            .unwrap_err();
        assert_error(
            py,
            error,
            py.get_type::<PyRuntimeError>().as_any(),
            "is not usable",
        );
    });
}

#[test]
fn second_mutant_of_same_file_never_reads_first_bytecode() {
    let case = Case::new();
    let source = case.write("m.py", MODULE_ONE);
    let original = fs::metadata(&source).unwrap();
    let plain = plain_env();
    let scratch = case.root().join("scratch");
    let first = isolated_env(&plain, &scratch, &[&source]);
    assert_eq!(child(&first, case.root(), CHILD_F), "1");
    pinned_mtime_write(&source, MODULE_THREE, &original);
    let second = isolated_env(&plain, &scratch, &[&source]);
    assert_eq!(child(&second, case.root(), CHILD_F), "3");
}

#[test]
fn unmutated_module_is_served_from_prefix_on_second_child() {
    let case = Case::new();
    case.write("n.py", "def g(): return 7\n");
    let source = case.write("m.py", MODULE_ONE);
    let original = fs::metadata(&source).unwrap();
    let plain = plain_env();
    let scratch = case.root().join("scratch");
    let first = isolated_env(&plain, &scratch, &[&source]);
    assert_eq!(child(&first, case.root(), CHILD_FG), "1 7");
    let n_cache = cache_paths(&case.root().join("n.py"), &scratch.join("pycache"))[0].clone();
    assert!(n_cache.is_file(), "first child must cache n in prefix");
    let before = fs::metadata(&n_cache).unwrap();
    let count = pyc_count(&scratch.join("pycache"));
    pinned_mtime_write(&source, MODULE_FOUR, &original);
    let second = isolated_env(&plain, &scratch, &[&source]);
    assert_eq!(child(&second, case.root(), CHILD_FG), "4 7");
    assert_eq!(pyc_count(&scratch.join("pycache")), count);
    let after = fs::metadata(&n_cache).unwrap();
    assert_eq!(
        (after.ino(), after.mtime(), after.mtime_nsec()),
        (before.ino(), before.mtime(), before.mtime_nsec())
    );
}

#[test]
fn eviction_refuses_to_leave_runs_own_scratch() {
    let case = Case::new();
    let source = case.write("proj/m.py", "x = 1\n");
    let run = case.root().join("run");
    let outside = case.mkdir("outside");
    let scratch = scratch_root(&run);
    let victim = cache_paths(&source, &scratch.join("pycache"))[0].clone();
    let mirrored = victim.parent().unwrap().parent().unwrap();
    fs::create_dir_all(mirrored.parent().unwrap()).unwrap();
    symlink(&outside, mirrored).unwrap();
    fs::create_dir_all(victim.parent().unwrap()).unwrap();
    fs::write(&victim, b"stale").unwrap();
    Python::attach(|py| {
        let error = module(py, "conductor.bytecode_isolation")
            .getattr("evict_mutated_caches")
            .unwrap()
            .call1((vec![path(py, &source)], path(py, &scratch)))
            .unwrap_err();
        assert_error(
            py,
            error,
            py.get_type::<PyRuntimeError>().as_any(),
            "not under the run's cache prefix",
        );
    });
    assert!(
        victim.is_file(),
        "refused eviction must not delete anything"
    );
}

#[test]
fn runs_scratch_root_is_one_pinned_name() {
    let case = Case::new();
    assert_eq!(
        scratch_root(case.root()),
        case.root().join(".bytecode-isolation")
    );
}

#[test]
fn eviction_reports_exactly_the_files_it_removed() {
    let case = Case::new();
    let source = case.write("m.py", "x = 1\n");
    let scratch = case.root().join("scratch");
    let caches = cache_paths(&source, &scratch.join("pycache"));
    for cache in &caches {
        fs::create_dir_all(cache.parent().unwrap()).unwrap();
        fs::write(cache, b"stale").unwrap();
    }
    Python::attach(|py| {
        let removed: Vec<PathBuf> = module(py, "conductor.bytecode_isolation")
            .getattr("evict_mutated_caches")
            .unwrap()
            .call1((vec![path(py, &source)], path(py, &scratch)))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(removed, caches);
    });
    assert!(caches.iter().all(|cache| !cache.exists()));
}
