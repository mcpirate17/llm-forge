#![cfg(feature = "python-compat-tests")]
//! Rust assertions over the Python project-context security boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)] // This helper is shared by three separate integration-test binaries.
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use support::{attr_text, module, path, text, AttrPatch, Case};

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
fn config_open_refuses_fifo_without_blocking() {
    let case = Case::new();
    let repo = repository(&case, "repository");
    let config = case.write("repository/.conductor/project.toml", "schema_version = 1\n");
    fs::remove_file(&config).unwrap();
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        ctx.getattr("os")
            .unwrap()
            .getattr("mkfifo")
            .unwrap()
            .call1((path(py, &config),))
            .unwrap();
        assert_eq!(
            error_code(py, &ctx, resolve(&ctx, &kwargs_project(py, &repo))),
            "CONFIG_IO"
        );
    });
}

#[test]
fn relative_git_topology_is_refused() {
    let case = Case::new();
    let relative = case.mkdir("relative");
    let git_dir = case.mkdir("git");
    let common_dir = case.mkdir("common");
    let _cwd = case.chdir(".");
    Python::attach(|py| {
        let ctx = module(py, "conductor.project_context");
        assert_relative_topology_is_refused(py, &ctx, &relative, &git_dir, &common_dir);
    });
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
