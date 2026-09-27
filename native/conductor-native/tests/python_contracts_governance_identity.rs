#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped governance lane identity API.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use support::{assert_error, module, path, text, Case};

const MAIN_BRANCH: &str = "codex/audit-rust-20260903";
const TREE_NAME: &str = "codex-rust-hotpath-next-20260903";

fn isolated_case() -> Case {
    let mut case = Case::new();
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        case.remove_env(name);
    }
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    case.set_env("GIT_CONFIG_SYSTEM", "/dev/null");
    case
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run fixture Git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn main_checkout(case: &Case) -> PathBuf {
    let repo = case.mkdir("LLM");
    git(&repo, &["init", "-q", "-b", MAIN_BRANCH]);
    repo
}

fn linked_shape(case: &Case, branch: &str) -> PathBuf {
    // `--separate-git-dir` creates a real .git file pointing at valid metadata.
    // It exercises the linked checkout shape without registering a worktree.
    let checkout = case.root().join(TREE_NAME);
    let metadata = case.root().join("separate-git-metadata");
    git(
        case.root(),
        &[
            "init",
            "-q",
            "--separate-git-dir",
            metadata.to_str().unwrap(),
            "-b",
            branch,
            checkout.to_str().unwrap(),
        ],
    );
    assert!(checkout.join(".git").is_file());
    checkout
}

fn identity<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.candidate_review.identity")
}

fn environment<'py>(py: Python<'py>, entries: &[(&str, &str)]) -> Bound<'py, PyDict> {
    let env = PyDict::new(py);
    for (name, value) in entries {
        env.set_item(name, value).unwrap();
    }
    env
}

fn resolved(py: Python<'_>, repo: &Path, entries: &[(&str, &str)]) -> String {
    text(
        &identity(py)
            .getattr("resolve_owner")
            .unwrap()
            .call1((path(py, repo), environment(py, entries)))
            .unwrap(),
    )
}

fn claim_cli(py: Python<'_>, repo: &Path, owner: Option<&str>) -> Output {
    let executable: String = PyModule::import(py, "sys")
        .unwrap()
        .getattr("executable")
        .unwrap()
        .extract()
        .unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("src")
        .canonicalize()
        .unwrap();
    let mut python_path = source.into_os_string();
    if let Some(inherited) = std::env::var_os("PYTHONPATH") {
        python_path.push(":");
        python_path.push(inherited);
    }
    let mut command = Command::new(executable);
    command
        .args(["-m", "conductor.candidate_review.cli", "claim", "--repo"])
        .arg(repo)
        .args([
            "--justification",
            "identity contract",
            "--expected-minutes",
            "5",
            "--max-minutes",
            "10",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .env("PYTHONPATH", python_path)
        .env("GOVERNANCE_OWNER", "")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("CUDA_VISIBLE_DEVICES", "")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES");
    if let Some(owner) = owner {
        command.args(["--owner", owner]);
    }
    command.arg("pkg/mod.py").output().expect("run claim CLI")
}

#[test]
fn a_worktree_is_named_for_itself_not_its_branch() {
    let case = isolated_case();
    let tree = linked_shape(&case, "codex/other-20260903");
    Python::attach(|py| {
        let lane = identity(py)
            .getattr("lane_of")
            .unwrap()
            .call1((path(py, &tree),))
            .unwrap();
        assert_eq!(text(&lane), TREE_NAME);
    });
}

#[test]
fn the_main_checkout_falls_back_to_its_branch() {
    let case = isolated_case();
    let repo = main_checkout(&case);
    Python::attach(|py| {
        let lane = identity(py)
            .getattr("lane_of")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        assert_eq!(text(&lane), "codex-audit-rust-20260903");
    });
}

#[test]
fn a_detached_checkout_has_no_lane() {
    let case = isolated_case();
    let repo = main_checkout(&case);
    fs::write(repo.join(".git/HEAD"), format!("{}\n", "a".repeat(40))).unwrap();
    Python::attach(|py| {
        let lane = identity(py)
            .getattr("lane_of")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        assert_eq!(text(&lane), "");
    });
}

#[test]
fn the_vendor_pin_that_broke_codex_is_ignored() {
    let case = isolated_case();
    let tree = linked_shape(&case, "codex/hot-20260903");
    Python::attach(|py| {
        assert_eq!(
            resolved(
                py,
                &tree,
                &[
                    ("GOVERNANCE_OWNER", "codex"),
                    ("CODEX_HOME", "/home/tim/.codex")
                ]
            ),
            TREE_NAME
        );
    });
}

#[test]
fn a_real_declaration_still_wins() {
    let case = isolated_case();
    let repo = main_checkout(&case);
    Python::attach(|py| {
        assert_eq!(
            resolved(
                py,
                &repo,
                &[("GOVERNANCE_OWNER", "glm-adaptation-rust-20260902")]
            ),
            "glm-adaptation-rust-20260902"
        );
    });
}

#[test]
fn the_vendor_only_stands_in_when_no_lane_can_be_derived() {
    let case = isolated_case();
    let repo = main_checkout(&case);
    fs::write(repo.join(".git/HEAD"), format!("{}\n", "a".repeat(40))).unwrap();
    Python::attach(|py| assert_eq!(resolved(py, &repo, &[("CODEX_HOME", "/x")]), "codex"));
}

#[test]
fn an_unnameable_lane_raises_rather_than_guessing() {
    let case = isolated_case();
    let repo = main_checkout(&case);
    fs::write(repo.join(".git/HEAD"), format!("{}\n", "a".repeat(40))).unwrap();
    Python::attach(|py| {
        let identity = identity(py);
        let error = identity
            .getattr("resolve_owner")
            .unwrap()
            .call1((path(py, &repo), environment(py, &[])))
            .unwrap_err();
        assert_error(
            py,
            error,
            &identity.getattr("OwnerIdentityError").unwrap(),
            "no governance identity",
        );
    });
}

fn vendor_name_is_refused(vendor: &str) {
    let _case = isolated_case();
    Python::attach(|py| {
        let identity = identity(py);
        assert!(identity
            .getattr("is_vendor")
            .unwrap()
            .call1((vendor,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let error = identity
            .getattr("require_lane_owner")
            .unwrap()
            .call1((vendor,))
            .unwrap_err();
        assert_error(
            py,
            error,
            &identity.getattr("OwnerIdentityError").unwrap(),
            "names a vendor",
        );
    });
}

macro_rules! vendor_case {
    ($name:ident, $vendor:literal) => {
        #[test]
        fn $name() {
            vendor_name_is_refused($vendor);
        }
    };
}

vendor_case!(a_vendor_name_is_refused_claude, "claude");
vendor_case!(a_vendor_name_is_refused_codex, "codex");
vendor_case!(a_vendor_name_is_refused_qwen, "qwen");
vendor_case!(a_vendor_name_is_refused_grok, "grok");
vendor_case!(a_vendor_name_is_refused_mixed_case_codex, "CoDeX");
vendor_case!(a_vendor_name_is_refused_padded_codex, " codex ");

#[test]
fn a_lane_name_is_accepted_and_folded() {
    let _case = isolated_case();
    Python::attach(|py| {
        let folded = identity(py)
            .getattr("require_lane_owner")
            .unwrap()
            .call1(("Codex/Rust Hotpath 20260903",))
            .unwrap();
        assert_eq!(text(&folded), "codex-rust-hotpath-20260903");
    });
}

#[test]
fn normalize_drops_what_the_owner_charset_cannot_hold() {
    let _case = isolated_case();
    Python::attach(|py| {
        let normalize = identity(py).getattr("normalize").unwrap();
        assert_eq!(
            text(&normalize.call1(("  claude/claim@expected  ",)).unwrap()),
            "claude-claim-expected"
        );
        assert_eq!(text(&normalize.call1(("---",)).unwrap()), "");
        assert_eq!(
            text(&normalize.call1(("x".repeat(200),)).unwrap()).len(),
            64
        );
    });
}

#[test]
fn the_legacy_vendor_of_a_lane_survives_a_bare_shell() {
    let _case = isolated_case();
    Python::attach(|py| {
        let vendor_for = identity(py).getattr("vendor_for").unwrap();
        for (owner, env, expected) in [
            ("codex-rust-hotpath-next-20260903", &[][..], "codex"),
            (
                "branch-policy",
                &[("CLAUDE_PROJECT_DIR", "/x")][..],
                "claude",
            ),
            ("branch-policy", &[][..], ""),
        ] {
            assert_eq!(
                text(&vendor_for.call1((owner, environment(py, env))).unwrap()),
                expected
            );
        }
    });
}

#[test]
fn a_claim_defaults_to_the_lane_that_will_write_it() {
    let case = isolated_case();
    let repo = main_checkout(&case);
    Python::attach(|py| {
        let output = claim_cli(py, &repo, None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("owner=codex-audit-rust-20260903"));
    });
}

#[test]
fn claiming_as_another_lane_is_refused() {
    let case = isolated_case();
    let repo = main_checkout(&case);
    Python::attach(|py| {
        let output = claim_cli(py, &repo, Some(TREE_NAME));
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("is not this lane"));
    });
}

#[test]
fn claiming_as_a_bare_vendor_is_refused() {
    let case = isolated_case();
    let repo = main_checkout(&case);
    Python::attach(|py| {
        let output = claim_cli(py, &repo, Some("codex"));
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("names a vendor"));
    });
}
