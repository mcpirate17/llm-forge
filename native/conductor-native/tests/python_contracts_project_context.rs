#![cfg(feature = "python-compat-tests")]
//! Rust assertions over the Python project-context security boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)] // This helper is shared by three separate integration-test binaries.
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use support::{attr_text, module, path, text, AttrPatch, Case};

#[derive(Default)]
struct SelectorState {
    registered: HashMap<i32, i32>,
    closed: bool,
}

#[pyclass]
struct MockSelector(Arc<Mutex<SelectorState>>);

type MockProcessParts = (
    Py<PyAny>,
    Vec<std::thread::JoinHandle<()>>,
    Arc<Mutex<Vec<i32>>>,
);

#[pymethods]
impl MockSelector {
    fn register(&self, descriptor: i32, events: i32) {
        let mut state = self.0.lock().unwrap();
        assert!(!state.closed && !state.registered.contains_key(&descriptor));
        state.registered.insert(descriptor, events);
    }

    fn unregister(&self, descriptor: i32) {
        let mut state = self.0.lock().unwrap();
        assert!(!state.closed);
        state
            .registered
            .remove(&descriptor)
            .expect("registered descriptor");
    }

    fn select(&self, _timeout: f64) -> Vec<()> {
        assert!(!self.0.lock().unwrap().closed);
        Vec::new()
    }

    fn close(&self) {
        let mut state = self.0.lock().unwrap();
        state.registered.clear();
        state.closed = true;
    }
}

fn pipe_reader(
    py: Python<'_>,
    initial: Option<Vec<u8>>,
) -> (Py<PyAny>, Option<std::thread::JoinHandle<()>>, Option<i32>) {
    let os = PyModule::import(py, "os").unwrap();
    let (read_fd, write_fd): (i32, i32) = os
        .getattr("pipe")
        .unwrap()
        .call0()
        .unwrap()
        .extract()
        .unwrap();
    let reader = os
        .getattr("fdopen")
        .unwrap()
        .call1((read_fd, "rb"))
        .unwrap()
        .unbind();
    match initial {
        Some(bytes) => {
            let writer = std::thread::spawn(move || {
                // Ownership of the write descriptor moves to the writer thread.
                let mut file = unsafe { fs::File::from_raw_fd(write_fd) };
                let _ = file.write_all(&bytes);
            });
            (reader, Some(writer), None)
        }
        None => (reader, None, Some(write_fd)),
    }
}

fn mock_process(py: Python<'_>, stdout: Option<Vec<u8>>, pid: i32) -> MockProcessParts {
    let (stdout_reader, stdout_writer, open_stdout) = pipe_reader(py, stdout);
    let (stderr_reader, stderr_writer, open_stderr) = pipe_reader(py, Some(Vec::new()));
    let mut writers = Vec::new();
    writers.extend(stdout_writer);
    writers.extend(stderr_writer);
    let open_fds = Arc::new(Mutex::new(
        open_stdout
            .into_iter()
            .chain(open_stderr)
            .collect::<Vec<_>>(),
    ));
    let wait_fds = Arc::clone(&open_fds);
    let wait = PyCFunction::new_closure(
        py,
        None,
        None,
        move |_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<i32> {
            for fd in wait_fds.lock().unwrap().drain(..) {
                // The timeout path reaps the fake process just as the real path does.
                drop(unsafe { fs::File::from_raw_fd(fd) });
            }
            Ok(0)
        },
    )
    .unwrap();
    let process = PyModule::import(py, "types")
        .unwrap()
        .getattr("SimpleNamespace")
        .unwrap()
        .call0()
        .unwrap();
    process.setattr("pid", pid).unwrap();
    process.setattr("returncode", 0).unwrap();
    process.setattr("stdout", stdout_reader.bind(py)).unwrap();
    process.setattr("stderr", stderr_reader.bind(py)).unwrap();
    process.setattr("wait", wait).unwrap();
    (process.unbind(), writers, open_fds)
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run isolated fixture git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 git output")
}

fn repository(case: &Case, name: &str) -> PathBuf {
    let repo = case.mkdir(name);
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    git(&repo, &["config", "user.name", "fixture"]);
    fs::write(repo.join("tracked.txt"), "fixture\n").unwrap();
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-qm", "fixture"]);
    repo
}

fn kwargs_project<'py>(py: Python<'py>, project: &Path) -> Bound<'py, PyDict> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("project", path(py, project)).unwrap();
    kwargs
}

fn resolve<'py>(
    ctx: &Bound<'py, PyModule>,
    kwargs: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    ctx.getattr("resolve_project_context")?
        .call((), Some(kwargs))
}

fn error_code(
    py: Python<'_>,
    ctx: &Bound<'_, PyModule>,
    result: PyResult<Bound<'_, PyAny>>,
) -> String {
    let error = result.expect_err("project-context refusal expected");
    assert!(error
        .matches(py, ctx.getattr("ContextError").unwrap())
        .unwrap());
    error
        .value(py)
        .getattr("detail")
        .unwrap()
        .getattr("code")
        .unwrap()
        .extract()
        .unwrap()
}

fn sha_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}

fn collect_tree(root: &Path, current: &Path, entries: &mut Vec<(String, Option<Vec<u8>>)>) {
    let mut children: Vec<_> = fs::read_dir(current)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    children.sort();
    for child in children {
        let relative = child
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let metadata = fs::symlink_metadata(&child).unwrap();
        let bytes = if metadata.is_file() {
            Some(fs::read(&child).unwrap())
        } else {
            None
        };
        entries.push((relative, bytes));
        if metadata.is_dir() {
            collect_tree(root, &child, entries);
        }
    }
}

fn tree_digest(root: &Path) -> String {
    let mut entries = Vec::new();
    collect_tree(root, root, &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut digest = Sha256::new();
    for (relative, bytes) in entries {
        digest.update(relative.as_bytes());
        if let Some(bytes) = bytes {
            digest.update(bytes);
        }
    }
    let mut hex = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}

#[test]
fn git_identity_is_immutable_has_complete_provenance_and_does_not_write() {
    let case = Case::new();
    let repo = repository(&case, "repository");
    let nested = case.mkdir("repository/nested");
    let before = tree_digest(&repo);
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        let kwargs = kwargs_project(py, &nested);
        let poisoned = PyDict::new(py);
        poisoned.set_item("GIT_DIR", "/poisoned").unwrap();
        poisoned.set_item("GIT_WORK_TREE", "/poisoned").unwrap();
        kwargs.set_item("environment", poisoned).unwrap();
        let context = resolve(&ctx, &kwargs).unwrap();
        let expected_root = repo.canonicalize().unwrap().display().to_string();
        assert_eq!(attr_text(&context, "repo_root"), expected_root);
        assert_eq!(attr_text(&context, "worktree_root"), expected_root);
        let git_dir = context.getattr("git_dir").unwrap();
        let common_dir = context.getattr("git_common_dir").unwrap();
        assert!(!git_dir.is_none() && !common_dir.is_none());
        let repository_key = attr_text(&context, "repository_key");
        let worktree_key = attr_text(&context, "worktree_key");
        let expected_repository = sha_hex(
            &[
                b"conductor.repository.v1\0".as_slice(),
                text(&common_dir).as_bytes(),
            ]
            .concat(),
        );
        let expected_worktree = sha_hex(
            &[
                b"conductor.worktree.v1\0".as_slice(),
                text(&git_dir).as_bytes(),
                b"\0",
                expected_root.as_bytes(),
            ]
            .concat(),
        );
        assert_eq!(repository_key, format!("repo-v1-{expected_repository}"));
        assert_eq!(worktree_key, format!("wt-v1-{expected_worktree}"));
        assert_eq!(attr_text(&context, "project_id"), repository_key);
        assert!(!context
            .getattr("state_dir")
            .unwrap()
            .call_method0("exists")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let provenance = context.getattr("provenance").unwrap();
        let fields = context.getattr("__dataclass_fields__").unwrap();
        let actual_fields: Vec<String> = provenance
            .try_iter()
            .unwrap()
            .map(|item| attr_text(&item.unwrap(), "field"))
            .collect();
        let expected_fields: Vec<String> = fields
            .try_iter()
            .unwrap()
            .map(|item| item.unwrap().extract::<String>().unwrap())
            .filter(|field| field != "provenance")
            .collect();
        assert_eq!(actual_fields, expected_fields);
        for item in provenance.try_iter().unwrap() {
            assert!(item.unwrap().getattr("config_sha256").unwrap().is_none());
        }
        let set_error = context.setattr("repository_key", "other").unwrap_err();
        let builtins = PyModule::import(py, "builtins").unwrap();
        assert!(
            set_error
                .matches(py, builtins.getattr("AttributeError").unwrap())
                .unwrap()
                || set_error
                    .matches(py, builtins.getattr("TypeError").unwrap())
                    .unwrap()
        );
    });
    assert_eq!(tree_digest(&repo), before);
}

#[test]
fn explicit_project_environment_and_read_only_refusals() {
    let case = Case::new();
    let first = repository(&case, "first");
    let second = repository(&case, "second");
    let plain = case.mkdir("plain");
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        let kwargs = kwargs_project(py, &first);
        let environment = PyDict::new(py);
        environment
            .set_item("CONDUCTOR_PROJECT_DIR", second.to_str().unwrap())
            .unwrap();
        kwargs.set_item("environment", environment).unwrap();
        assert_eq!(
            attr_text(&resolve(&ctx, &kwargs).unwrap(), "repo_root"),
            first.display().to_string()
        );

        for (value, expected) in [("", "INVALID_ARGUMENT"), ("relative", "INVALID_PATH")] {
            let kwargs = PyDict::new(py);
            kwargs.set_item("start_dir", path(py, &first)).unwrap();
            let environment = PyDict::new(py);
            environment
                .set_item("CONDUCTOR_PROJECT_DIR", value)
                .unwrap();
            kwargs.set_item("environment", environment).unwrap();
            assert_eq!(error_code(py, &ctx, resolve(&ctx, &kwargs)), expected);
        }

        let kwargs = kwargs_project(py, &plain);
        kwargs.set_item("mode", "read_only").unwrap();
        let no_git = PyCFunction::new_closure(
            py,
            None,
            None,
            |_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
                Err(pyo3::exceptions::PyAssertionError::new_err(
                    "read-only mode must not invoke Git",
                ))
            },
        )
        .unwrap();
        let patch_git = AttrPatch::replace(&ctx, "_git_paths", no_git.as_any());
        let context = resolve(&ctx, &kwargs).unwrap();
        drop(patch_git);
        assert_eq!(
            attr_text(&context, "repo_root"),
            plain.display().to_string()
        );
        for field in [
            "git_dir",
            "git_common_dir",
            "repository_key",
            "worktree_key",
            "state_dir",
            "cache_dir",
            "artifact_dir",
        ] {
            assert!(
                context.getattr(field).unwrap().is_none(),
                "{field} should be absent"
            );
        }
        assert_eq!(
            error_code(
                py,
                &ctx,
                ctx.getattr("require_git_context")
                    .unwrap()
                    .call1((&context,))
            ),
            "UNSUPPORTED_REPOSITORY"
        );
        let missing_project = PyDict::new(py);
        missing_project
            .set_item("start_dir", path(py, &plain))
            .unwrap();
        missing_project.set_item("mode", "read_only").unwrap();
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &missing_project)),
            "INVALID_ARGUMENT"
        );
    });
}

#[test]
fn config_schema_paths_external_notes_and_digest() {
    let case = Case::new();
    let repo = repository(&case, "repository");
    let policy = case.write("repository/.conductor/policy.toml", "policy\n");
    let registry = case.write("repository/.conductor/registry.json", "{}\n");
    let notes = case.mkdir("repository/notes");
    let outside = case.mkdir("outside-notes");
    let config = case.write("repository/.conductor/project.toml", "schema_version = 1\n[project]\nid = \"same-label\"\n[paths]\npolicy = \"policy.toml\"\nregistry = \"registry.json\"\nnotes = [\"../notes\", \"../notes\", \"../../outside-notes\"]\n");
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        let kwargs = kwargs_project(py, &repo);
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &kwargs)),
            "PATH_OUTSIDE_PROJECT"
        );
        kwargs.set_item("start_dir", path(py, &repo)).unwrap();
        kwargs
            .set_item(
                "allowed_external_notes_roots",
                (path(py, Path::new("../outside-notes")),),
            )
            .unwrap();
        let context = resolve(&ctx, &kwargs).unwrap();
        assert_eq!(attr_text(&context, "project_id"), "same-label");
        assert_eq!(
            attr_text(&context, "policy_path"),
            policy.display().to_string()
        );
        assert_eq!(
            attr_text(&context, "registry_path"),
            registry.display().to_string()
        );
        let roots: Vec<String> = context
            .getattr("notes_roots")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|item| text(&item.unwrap()))
            .collect();
        assert_eq!(
            roots,
            [notes.display().to_string(), outside.display().to_string()]
        );
        let expected_digest = sha_hex(&fs::read(&config).unwrap());
        let provenance = context.getattr("provenance").unwrap();
        let config_row = provenance
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .find(|item| attr_text(item, "field") == "config_path")
            .unwrap();
        assert_eq!(attr_text(&config_row, "config_sha256"), expected_digest);

        fs::write(&config, "schema_version = true\n").unwrap();
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &kwargs_project(py, &repo))),
            "CONFIG_SCHEMA"
        );
        fs::write(&config, "schema_version = 1\n").unwrap();
        let outside_file = case.write("outside-reference.toml", "outside\n");
        for field in ["policy", "registry"] {
            let kwargs = kwargs_project(py, &repo);
            kwargs.set_item(field, path(py, &outside_file)).unwrap();
            assert_eq!(
                error_code(py, &ctx, resolve(&ctx, &kwargs)),
                "PATH_OUTSIDE_PROJECT"
            );
        }
        fs::write(
            &config,
            "schema_version = 1\n[project]\nid = \"valid\u{0085}but-control\"\n",
        )
        .unwrap();
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &kwargs_project(py, &repo))),
            "CONFIG_SCHEMA"
        );
    });
}

#[test]
fn config_symlink_escape_and_missing_targets_are_refused() {
    let case = Case::new();
    let repo = repository(&case, "repository");
    let outside = case.write("outside.toml", "schema_version = 1\n");
    let holder = case.mkdir("repository/.conductor");
    let linked = holder.join("project.toml");
    std::os::unix::fs::symlink(&outside, &linked).unwrap();
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &kwargs_project(py, &repo))),
            "PATH_OUTSIDE_PROJECT"
        );
        let kwargs = kwargs_project(py, &repo);
        kwargs.set_item("config", path(py, &outside)).unwrap();
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &kwargs)),
            "PATH_OUTSIDE_PROJECT"
        );
        let kwargs = kwargs_project(py, &repo);
        kwargs
            .set_item("config", path(py, &repo.join("missing.toml")))
            .unwrap();
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &kwargs)),
            "CONFIG_NOT_FOUND"
        );
        fs::remove_file(&linked).unwrap();
        std::os::unix::fs::symlink(repo.join("missing-target.toml"), &linked).unwrap();
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &kwargs_project(py, &repo))),
            "CONFIG_IO"
        );
        fs::remove_file(&linked).unwrap();
        let controlled = case.write("repository/control\nconfig.toml", "schema_version = 1\n");
        std::os::unix::fs::symlink(controlled, &linked).unwrap();
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &kwargs_project(py, &repo))),
            "INVALID_PATH"
        );
        fs::remove_file(&linked).unwrap();
        fs::remove_dir(&holder).unwrap();
        std::os::unix::fs::symlink(repo.join("missing-config-directory"), &holder).unwrap();
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &kwargs_project(py, &repo))),
            "CONFIG_IO"
        );
    });
}

#[test]
fn config_open_refuses_fifo_and_swapped_ancestor() {
    let case = Case::new();
    let repo = repository(&case, "repository");
    let config = case.write("repository/.conductor/project.toml", "schema_version = 1\n");
    let holder = repo.join(".conductor");
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        let os = ctx.getattr("os").unwrap();
        let original = os.getattr("open").unwrap().unbind();
        let mkfifo = os.getattr("mkfifo").unwrap().unbind();
        let changed = Arc::new(AtomicBool::new(false));
        let changed_in_callback = Arc::clone(&changed);
        let config_in_callback = config.clone();
        let open = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  kwargs: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                let py = args.py();
                let relative_open = match kwargs {
                    Some(dict) => dict
                        .get_item("dir_fd")?
                        .is_some_and(|value| !value.is_none()),
                    None => false,
                };
                if relative_open && !changed_in_callback.swap(true, Ordering::SeqCst) {
                    fs::remove_file(&config_in_callback).unwrap();
                    mkfifo
                        .bind(py)
                        .call1((config_in_callback.to_str().unwrap(),))?;
                }
                Ok(original.bind(py).call(args, kwargs)?.unbind())
            },
        )
        .unwrap();
        let patch = AttrPatch::replace(&os, "open", open.as_any());
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &kwargs_project(py, &repo))),
            "CONFIG_IO"
        );
        assert!(changed.load(Ordering::SeqCst));
        drop(patch);
    });

    fs::remove_file(&config).unwrap();
    fs::write(&config, "schema_version = 1\n").unwrap();
    let outside = case.mkdir("outside");
    case.write("outside/project.toml", "schema_version = 1\n");
    let moved = repo.join(".conductor-before-swap");
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        let os = ctx.getattr("os").unwrap();
        let original = os.getattr("open").unwrap().unbind();
        let changed = Arc::new(AtomicBool::new(false));
        let changed_in_callback = Arc::clone(&changed);
        let holder_in_callback = holder.clone();
        let moved_in_callback = moved.clone();
        let outside_in_callback = outside.clone();
        let open = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  kwargs: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                let py = args.py();
                let relative_open = match kwargs {
                    Some(dict) => dict
                        .get_item("dir_fd")?
                        .is_some_and(|value| !value.is_none()),
                    None => false,
                };
                if relative_open && !changed_in_callback.swap(true, Ordering::SeqCst) {
                    fs::rename(&holder_in_callback, &moved_in_callback).unwrap();
                    std::os::unix::fs::symlink(&outside_in_callback, &holder_in_callback).unwrap();
                }
                Ok(original.bind(py).call(args, kwargs)?.unbind())
            },
        )
        .unwrap();
        let patch = AttrPatch::replace(&os, "open", open.as_any());
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &kwargs_project(py, &repo))),
            "CONFIG_IO"
        );
        assert!(changed.load(Ordering::SeqCst));
        drop(patch);
    });
}

#[test]
fn git_probe_has_fixed_environment_and_output_cap() {
    let _case = Case::new();
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        assert_fixed_git_environment(py, &ctx);
        assert_git_output_cap(py, &ctx);
    });
}

fn assert_fixed_git_environment(py: Python<'_>, ctx: &Bound<'_, PyModule>) {
    let subprocess = ctx.getattr("subprocess").unwrap();
    let (process, writers, _) = mock_process(py, Some(b"ok\n".to_vec()), 73_421);
    let observed = Arc::new(Mutex::new(None::<(HashMap<String, String>, bool)>));
    let observed_in_callback = Arc::clone(&observed);
    let popen = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>,
              kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> {
            let py = args.py();
            let kwargs = kwargs.expect("Popen keyword arguments");
            let env: HashMap<String, String> =
                kwargs.get_item("env")?.expect("explicit env").extract()?;
            let new_session: bool = kwargs
                .get_item("start_new_session")?
                .expect("session flag")
                .extract()?;
            *observed_in_callback.lock().unwrap() = Some((env, new_session));
            Ok(process.clone_ref(py))
        },
    )
    .unwrap();
    let patch = AttrPatch::replace(&subprocess, "Popen", popen.as_any());
    let result: Vec<u8> = ctx
        .getattr("_bounded_git")
        .unwrap()
        .call1((("rev-parse",),))
        .unwrap()
        .extract()
        .unwrap();
    assert_eq!(result, b"ok\n");
    let (env, session) = observed.lock().unwrap().clone().expect("Popen invocation");
    assert_eq!(
        env,
        HashMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("LC_ALL".into(), "C".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
        ])
    );
    assert!(session);
    drop(patch);
    for writer in writers {
        writer.join().unwrap();
    }
}

fn assert_git_output_cap(py: Python<'_>, ctx: &Bound<'_, PyModule>) {
    let subprocess = ctx.getattr("subprocess").unwrap();
    let (process, writers, _) = mock_process(py, Some(vec![b'x'; 32 * 1024 + 1]), 73_421);
    let popen = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>,
              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> { Ok(process.clone_ref(args.py())) },
    )
    .unwrap();
    let patch_popen = AttrPatch::replace(&subprocess, "Popen", popen.as_any());
    let killed = Arc::new(Mutex::new(Vec::<(i32, i32)>::new()));
    let killed_in_callback = Arc::clone(&killed);
    let killpg = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
            killed_in_callback.lock().unwrap().push(args.extract()?);
            Ok(())
        },
    )
    .unwrap();
    let os = ctx.getattr("os").unwrap();
    let patch_kill = AttrPatch::replace(&os, "killpg", killpg.as_any());
    assert_eq!(
        error_code(
            py,
            ctx,
            ctx.getattr("_bounded_git")
                .unwrap()
                .call1((("rev-parse",),))
        ),
        "GIT_OUTPUT_LIMIT"
    );
    let sigkill: i32 = ctx
        .getattr("signal")
        .unwrap()
        .getattr("SIGKILL")
        .unwrap()
        .extract()
        .unwrap();
    assert_eq!(*killed.lock().unwrap(), [(73_421, sigkill)]);
    drop(patch_kill);
    drop(patch_popen);
    for writer in writers {
        writer.join().unwrap();
    }
}

#[test]
fn git_probe_timeout_reaps_and_relative_topology_is_refused() {
    let case = Case::new();
    let relative = case.mkdir("relative");
    let git_dir = case.mkdir("git");
    let common_dir = case.mkdir("common");
    let _cwd = case.chdir(".");
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        assert_git_timeout(py, &ctx);
        assert_relative_topology_is_refused(py, &ctx, &relative, &git_dir, &common_dir);
    });
}

fn assert_git_timeout(py: Python<'_>, ctx: &Bound<'_, PyModule>) {
    let subprocess = ctx.getattr("subprocess").unwrap();
    let (process, writers, open_fds) = mock_process(py, None, 73_422);
    let popen = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>,
              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> { Ok(process.clone_ref(args.py())) },
    )
    .unwrap();
    let patch_popen = AttrPatch::replace(&subprocess, "Popen", popen.as_any());
    let selector_state = Arc::new(Mutex::new(SelectorState::default()));
    let selector = Py::new(py, MockSelector(Arc::clone(&selector_state))).unwrap();
    let selector_factory = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>,
              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<MockSelector>> { Ok(selector.clone_ref(args.py())) },
    )
    .unwrap();
    let selectors = ctx.getattr("selectors").unwrap();
    let patch_selector =
        AttrPatch::replace(&selectors, "DefaultSelector", selector_factory.as_any());
    let time = ctx.getattr("time").unwrap();
    let ticks = Arc::new(Mutex::new(vec![4.0, 0.0]));
    let ticks_in_callback = Arc::clone(&ticks);
    let monotonic = PyCFunction::new_closure(
        py,
        None,
        None,
        move |_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<f64> {
            Ok(ticks_in_callback
                .lock()
                .unwrap()
                .pop()
                .expect("bounded monotonic calls"))
        },
    )
    .unwrap();
    let patch_time = AttrPatch::replace(&time, "monotonic", monotonic.as_any());
    let killed = Arc::new(Mutex::new(Vec::<i32>::new()));
    let killed_in_callback = Arc::clone(&killed);
    let killpg = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
            let (pid, _signal): (i32, i32) = args.extract()?;
            killed_in_callback.lock().unwrap().push(pid);
            Ok(())
        },
    )
    .unwrap();
    let os = ctx.getattr("os").unwrap();
    let patch_kill = AttrPatch::replace(&os, "killpg", killpg.as_any());
    assert_eq!(
        error_code(
            py,
            ctx,
            ctx.getattr("_bounded_git")
                .unwrap()
                .call1((("rev-parse",),))
        ),
        "GIT_TIMEOUT"
    );
    assert_eq!(*killed.lock().unwrap(), [73_422]);
    let state = selector_state.lock().unwrap();
    assert!(state.closed && state.registered.is_empty());
    drop(state);
    assert!(
        open_fds.lock().unwrap().is_empty(),
        "process.wait closed open pipe writers"
    );
    drop(patch_kill);
    drop(patch_time);
    drop(patch_selector);
    drop(patch_popen);
    for writer in writers {
        writer.join().unwrap();
    }
}

fn assert_relative_topology_is_refused(
    py: Python<'_>,
    ctx: &Bound<'_, PyModule>,
    relative: &Path,
    git_dir: &Path,
    common_dir: &Path,
) {
    let topology = format!(
        "relative\n{}\n{}\n",
        git_dir.display(),
        common_dir.display()
    )
    .into_bytes();
    let responses = Arc::new(Mutex::new(vec![
        topology.clone(),
        topology,
        b"true\nfalse\n".to_vec(),
    ]));
    let responses_in_callback = Arc::clone(&responses);
    let bounded = PyCFunction::new_closure(
        py,
        None,
        None,
        move |_args: &Bound<'_, PyTuple>,
              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Vec<u8>> {
            Ok(responses_in_callback
                .lock()
                .unwrap()
                .pop()
                .expect("Git topology response"))
        },
    )
    .unwrap();
    let patch_bounded = AttrPatch::replace(ctx, "_bounded_git", bounded.as_any());
    assert_eq!(
        error_code(
            py,
            ctx,
            ctx.getattr("_git_paths")
                .unwrap()
                .call1((path(py, relative),))
        ),
        "GIT_DISCOVERY_FAILED"
    );
    drop(patch_bounded);
}

#[test]
fn unrelated_repositories_and_linked_worktrees_have_distinct_keys() {
    let case = Case::new();
    let first = repository(&case, "first");
    let second = repository(&case, "second");
    for repo in [&first, &second] {
        fs::create_dir(repo.join(".conductor")).unwrap();
        fs::write(
            repo.join(".conductor/project.toml"),
            "schema_version = 1\n[project]\nid = \"same\"\n",
        )
        .unwrap();
    }
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        let one = resolve(&ctx, &kwargs_project(py, &first)).unwrap();
        let two = resolve(&ctx, &kwargs_project(py, &second)).unwrap();
        assert_eq!(attr_text(&one, "project_id"), "same");
        assert_eq!(attr_text(&two, "project_id"), "same");
        assert_ne!(
            attr_text(&one, "repository_key"),
            attr_text(&two, "repository_key")
        );
    });
    let linked = case.root().join("linked");
    git(
        &first,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        let one = resolve(&ctx, &kwargs_project(py, &first)).unwrap();
        let linked_context = resolve(&ctx, &kwargs_project(py, &linked)).unwrap();
        assert_eq!(
            attr_text(&one, "repository_key"),
            attr_text(&linked_context, "repository_key")
        );
        assert_ne!(
            attr_text(&one, "worktree_key"),
            attr_text(&linked_context, "worktree_key")
        );
        assert_ne!(
            attr_text(&one, "git_dir"),
            attr_text(&linked_context, "git_dir")
        );
    });
}

#[test]
fn submodule_and_separate_git_directory_keep_own_topology() {
    let case = Case::new();
    let leaf = repository(&case, "leaf");
    let superproject = repository(&case, "superproject");
    git(
        &superproject,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            leaf.to_str().unwrap(),
            "vendor/leaf",
        ],
    );
    git(&superproject, &["commit", "-qm", "submodule"]);
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        let submodule_path = superproject.join("vendor/leaf");
        let submodule = resolve(&ctx, &kwargs_project(py, &submodule_path)).unwrap();
        let parent = resolve(&ctx, &kwargs_project(py, &superproject)).unwrap();
        assert_eq!(
            attr_text(&submodule, "repo_root"),
            submodule_path.display().to_string()
        );
        assert_ne!(
            attr_text(&submodule, "repository_key"),
            attr_text(&parent, "repository_key")
        );
    });
    let separate = case.root().join("separate");
    let git_dir = case.root().join("separate-git");
    git(
        case.root(),
        &[
            "init",
            "-q",
            &format!("--separate-git-dir={}", git_dir.display()),
            separate.to_str().unwrap(),
        ],
    );
    git(
        &separate,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(&separate, &["config", "user.name", "fixture"]);
    fs::write(separate.join("tracked"), "x").unwrap();
    git(&separate, &["add", "tracked"]);
    git(&separate, &["commit", "-qm", "fixture"]);
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        let separate_context = resolve(&ctx, &kwargs_project(py, &separate)).unwrap();
        assert_eq!(
            attr_text(&separate_context, "git_dir"),
            git_dir.display().to_string()
        );
    });
}
