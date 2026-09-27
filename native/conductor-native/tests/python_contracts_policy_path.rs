#![cfg(feature = "python-compat-tests")]
//! Rust-owned path precedence and candidate isolation contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use support::{assert_error, module, path, text, Case};

fn policy_env() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        Python::attach(|py| {
            text(
                &module(py, "conductor.candidate_review.policy_path")
                    .getattr("POLICY_ENV")
                    .unwrap(),
            )
        })
    })
}

fn clean_case() -> Case {
    let mut case = Case::new();
    case.remove_env(policy_env());
    case
}

fn policy(root: &Path, relative: &str) -> PathBuf {
    let file = root.join(relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "schema_version = 1\n").unwrap();
    file
}

fn default_relative(source: &Bound<'_, PyModule>) -> String {
    text(&source.getattr("DEFAULT_POLICY_RELATIVE").unwrap())
}

fn package(source: &Bound<'_, PyModule>) -> String {
    text(&source.getattr("PACKAGE_POLICY").unwrap())
}

fn resolve_with_tree<'py>(
    py: Python<'py>,
    source: &Bound<'py, PyModule>,
    explicit: Option<&str>,
    tree: &Path,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("tree", path(py, tree)).unwrap();
    let call = source.getattr("resolve_policy_path").unwrap();
    match explicit {
        Some(raw) => call.call((raw,), Some(&kwargs)),
        None => call.call((), Some(&kwargs)),
    }
}

#[test]
fn explicit_flag_beats_environment() {
    let mut case = clean_case();
    let flag = policy(case.root(), "flag.toml");
    let env = policy(case.root(), "env.toml");
    case.set_env(policy_env(), env.to_str().unwrap());
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.policy_path");
        let resolve = source.getattr("resolve_policy_path").unwrap();
        assert_eq!(
            text(&resolve.call1((path(py, &flag),)).unwrap()),
            flag.display().to_string()
        );
        assert_eq!(
            text(&resolve.call1((flag.to_str().unwrap(),)).unwrap()),
            flag.display().to_string()
        );
    });
}

#[test]
fn environment_beats_the_default() {
    let mut case = clean_case();
    let env = policy(case.root(), "env.toml");
    case.set_env(policy_env(), env.to_str().unwrap());
    let _cwd = case.chdir("");
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.policy_path");
        policy(case.root(), &default_relative(&source));
        assert_eq!(
            text(
                &source
                    .getattr("resolve_policy_path")
                    .unwrap()
                    .call0()
                    .unwrap()
            ),
            env.display().to_string()
        );
    });
}

#[test]
fn empty_flag_and_blank_environment_mean_default() {
    let mut case = clean_case();
    case.mkdir(".git");
    case.set_env(policy_env(), "   ");
    let _cwd = case.chdir("");
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.policy_path");
        let default = policy(case.root(), &default_relative(&source));
        assert_eq!(
            text(
                &source
                    .getattr("resolve_policy_path")
                    .unwrap()
                    .call1(("",))
                    .unwrap()
            ),
            default.display().to_string()
        );
    });
}

#[test]
fn default_is_the_enclosing_repo_from_a_subdirectory() {
    let case = clean_case();
    case.write(".git", "gitdir: elsewhere\n");
    let _cwd = case.chdir("a/b");
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.policy_path");
        let default = policy(case.root(), &default_relative(&source));
        assert_eq!(
            text(
                &source
                    .getattr("resolve_policy_path")
                    .unwrap()
                    .call0()
                    .unwrap()
            ),
            default.display().to_string()
        );
    });
}

#[test]
fn default_outside_any_repo_is_the_package_policy() {
    let case = clean_case();
    let _cwd = case.chdir("");
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.policy_path");
        let root = path(py, case.root());
        let enclosing = source
            .getattr("enclosing_repo")
            .unwrap()
            .call1((&root,))
            .unwrap();
        let default = case.root().join(default_relative(&source));
        assert!(enclosing.is_none() || !default.exists());
        assert_eq!(
            text(
                &source
                    .getattr("resolve_policy_path")
                    .unwrap()
                    .call0()
                    .unwrap()
            ),
            package(&source)
        );
        let packaged = source.getattr("PACKAGE_POLICY").unwrap();
        assert_eq!(
            text(&packaged.getattr("name").unwrap()),
            "candidate_policy.toml"
        );
        assert_eq!(
            text(&packaged.getattr("parent").unwrap().getattr("name").unwrap()),
            "conductor"
        );
    });
}

#[test]
fn repo_without_a_policy_falls_through_to_the_package() {
    let case = clean_case();
    case.mkdir(".git");
    let _cwd = case.chdir("");
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.policy_path");
        assert_eq!(
            text(
                &source
                    .getattr("resolve_policy_path")
                    .unwrap()
                    .call0()
                    .unwrap()
            ),
            package(&source)
        );
    });
}

#[test]
fn missing_explicit_or_environment_path_fails_loud() {
    let mut case = clean_case();
    let missing = case.root().join("absent.toml");
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.policy_path");
        let error_type = module(py, "conductor.candidate_review.policy")
            .getattr("PolicyError")
            .unwrap();
        let resolve = source.getattr("resolve_policy_path").unwrap();
        let explicit = resolve.call1((path(py, &missing),)).unwrap_err();
        assert_error(py, explicit, &error_type, "no candidate policy (--policy)");
        case.set_env(policy_env(), missing.to_str().unwrap());
        let environment = resolve.call0().unwrap_err();
        assert_error(
            py,
            environment,
            &error_type,
            &format!("({}); tried: {}", policy_env(), missing.display()),
        );
    });
}

#[test]
fn tree_default_is_joined_to_the_tree() {
    let case = clean_case();
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.policy_path");
        let default = policy(case.root(), &default_relative(&source));
        assert_eq!(
            text(&resolve_with_tree(py, &source, None, case.root()).unwrap()),
            default.display().to_string()
        );
    });
}

#[test]
fn tree_explicit_and_environment_are_tree_relative() {
    let mut case = clean_case();
    let flag = policy(case.root(), "flag/policy.toml");
    let env = policy(case.root(), "env/policy.toml");
    case.set_env(policy_env(), "env/policy.toml");
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.policy_path");
        assert_eq!(
            text(&resolve_with_tree(py, &source, None, case.root()).unwrap()),
            env.display().to_string()
        );
        assert_eq!(
            text(&resolve_with_tree(py, &source, Some("flag/policy.toml"), case.root()).unwrap()),
            flag.display().to_string()
        );
    });
}

fn tree_rejects_escape(raw: &str) {
    let case = clean_case();
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.policy_path");
        policy(case.root(), &default_relative(&source));
        let error = resolve_with_tree(py, &source, Some(raw), case.root()).unwrap_err();
        let error_type = module(py, "conductor.candidate_review.policy")
            .getattr("PolicyError")
            .unwrap();
        assert_error(py, error, &error_type, "must be candidate-relative");
    });
}

macro_rules! escape_case {
    ($name:ident, $raw:expr) => {
        #[test]
        fn $name() {
            tree_rejects_escape($raw);
        }
    };
}

escape_case!(tree_rejects_absolute_path, "/etc/policy.toml");
escape_case!(tree_rejects_parent_path, "../policy.toml");
escape_case!(tree_rejects_nested_parent_escape, "a/../../p");

#[test]
fn tree_without_a_policy_never_falls_back_to_the_package() {
    let case = clean_case();
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.policy_path");
        let packaged = source.getattr("PACKAGE_POLICY").unwrap();
        assert!(packaged
            .call_method0("is_file")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let resolved = resolve_with_tree(py, &source, None, case.root()).unwrap();
        let default = case.root().join(default_relative(&source));
        assert_eq!(text(&resolved), default.display().to_string());
        assert!(!default.exists());
        let review = module(py, "conductor.candidate_review.policy");
        let error = review
            .getattr("load_policy")
            .unwrap()
            .call1((&resolved,))
            .unwrap_err();
        assert_error(
            py,
            error,
            &review.getattr("PolicyError").unwrap(),
            "required candidate policy is unreadable",
        );
    });
}

#[test]
fn enclosing_repo_stops_at_the_nearest_git_marker() {
    let case = clean_case();
    case.mkdir("outer/.git");
    case.mkdir("outer/inner/deep");
    case.write("outer/inner/.git", "gitdir: x\n");
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.policy_path");
        let enclosing = source.getattr("enclosing_repo").unwrap();
        let outer = case.root().join("outer");
        let inner = outer.join("inner");
        assert_eq!(
            text(&enclosing.call1((path(py, &inner.join("deep")),)).unwrap()),
            inner.display().to_string()
        );
        assert_eq!(
            text(&enclosing.call1((path(py, &outer.join("other")),)).unwrap()),
            outer.display().to_string()
        );
    });
}
