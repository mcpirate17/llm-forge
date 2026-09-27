#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the candidate-review ownership claims view.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{module, path, text, AttrPatch, Case};

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
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
    ] {
        case.remove_env(name);
    }
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    case.set_env("GIT_CONFIG_SYSTEM", "/dev/null");
    case
}

fn init_repo(case: &Case) -> PathBuf {
    let repo = case.mkdir("repo");
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(&repo)
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
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet", "--initial-branch=main"]);
    git(&["config", "user.name", "Candidate Review Test"]);
    git(&["config", "user.email", "candidate-review@example.invalid"]);
    repo
}

fn repo_with_claims(case: &Case) -> PathBuf {
    let repo = init_repo(case);
    fs::create_dir(repo.join("conductor")).unwrap();
    fs::write(repo.join("conductor/a.py"), "A = 1\n").unwrap();
    fs::write(repo.join("conductor/b.py"), "B = 1\n").unwrap();
    fs::write(repo.join("docs.md"), "# docs\n").unwrap();
    Python::attach(|py| {
        let own = module(py, "conductor.candidate_review.ownership");
        let create = own.getattr("create_claim").unwrap();
        for (owner, paths, justification) in [
            (
                "alpha",
                vec!["conductor/a.py", "conductor/b.py"],
                format!("alpha {}", "x".repeat(100)),
            ),
            ("beta", vec!["docs.md"], "beta docs".to_owned()),
        ] {
            let kwargs = PyDict::new(py);
            kwargs.set_item("owner", owner).unwrap();
            kwargs.set_item("paths", paths).unwrap();
            kwargs.set_item("justification", justification).unwrap();
            kwargs.set_item("max_minutes", 60).unwrap();
            create.call((path(py, &repo),), Some(&kwargs)).unwrap();
        }
    });
    repo
}

fn cli(repo: &Path, options: &[&str]) -> (i32, String) {
    Python::attach(|py| {
        let args = PyList::empty(py);
        for item in ["claims", "--repo", repo.to_str().unwrap()] {
            args.append(item).unwrap();
        }
        for item in options {
            args.append(item).unwrap();
        }
        let io = module(py, "io");
        let out = io.call_method0("StringIO").unwrap();
        let sys = module(py, "sys");
        let _patch = AttrPatch::replace(sys.as_any(), "stdout", &out);
        let code: i32 = module(py, "conductor.candidate_review.cli")
            .getattr("main")
            .unwrap()
            .call1((args,))
            .unwrap()
            .extract()
            .unwrap();
        (code, text(&out.call_method0("getvalue").unwrap()))
    })
}

#[test]
fn compact_view_is_one_line_per_claim_plus_paths() {
    let case = isolated_case();
    let repo = repo_with_claims(&case);
    let (code, out) = cli(&repo, &["--compact"]);
    assert_eq!(code, 0);
    let lines: Vec<_> = out.lines().collect();
    assert!(lines[0].starts_with("claims: 2 active (0 overrun), 0 expired, sha256 "));
    assert_eq!(lines.len(), 3);
    let alpha = lines.iter().find(|line| line.contains(" alpha ")).unwrap();
    assert!(alpha.contains(" 2 paths  conductor/(2)  ") && alpha.ends_with('…'));
    assert_eq!(alpha.rsplit("  ").next().unwrap().chars().count(), 72);
    let beta = lines.iter().find(|line| line.contains(" beta ")).unwrap();
    assert!(beta.contains(" 1 paths  .(1)  beta docs"));
    assert!(!out.contains("conductor/a.py"));
    assert!(out.len() < 400);
    let (code, out) = cli(&repo, &["--compact", "--paths"]);
    assert_eq!(code, 0);
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines.len(), 5);
    assert!(lines.contains(&"    conductor/a.py conductor/b.py"));
    assert!(lines.contains(&"    docs.md"));
}

#[test]
fn path_filter_applies_to_both_views() {
    let case = isolated_case();
    let repo = repo_with_claims(&case);
    let base = ["--path", "conductor/b.py"];
    let (code, out) = cli(&repo, &base);
    assert_eq!(code, 0);
    let payload: Value = serde_json::from_str(&out).unwrap();
    let owners: Vec<_> = payload["claims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|claim| claim["owner"].as_str().unwrap())
        .collect();
    assert_eq!(owners, ["alpha"]);
    let (code, out) = cli(&repo, &["--path", "conductor/b.py", "--compact"]);
    assert_eq!(code, 0);
    assert!(out.contains("claims: 1 active") && !out.contains("beta"));
    assert!(out
        .lines()
        .any(|line| line == "    conductor/a.py conductor/b.py"));
    let (code, out) = cli(
        &repo,
        &["--path", "conductor/b.py", "--path", "docs.md", "--compact"],
    );
    assert_eq!(code, 0);
    assert!(out.contains("claims: 2 active"));
    let (code, out) = cli(&repo, &["--path", "nothing/here.py", "--compact"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("claims: 0 active"));
}

#[test]
fn default_json_view_is_unchanged() {
    let case = isolated_case();
    let repo = repo_with_claims(&case);
    let (code, out) = cli(&repo, &[]);
    assert_eq!(code, 0);
    let payload: Value = serde_json::from_str(&out).unwrap();
    let fields: std::collections::BTreeSet<_> = payload
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(fields, ["claims", "sha256"].into_iter().collect());
    assert_eq!(payload["claims"].as_array().unwrap().len(), 2);
    let fields: std::collections::BTreeSet<_> = payload["claims"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        fields,
        [
            "claim_id",
            "owner",
            "paths",
            "justification",
            "created_at",
            "expires_at",
            "expected_at"
        ]
        .into_iter()
        .collect()
    );
}

fn fixed_now<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "datetime")
        .getattr("datetime")
        .unwrap()
        .call_method1("fromisoformat", ("2026-08-27T12:00:00+00:00",))
        .unwrap()
}

fn claim<'py>(
    py: Python<'py>,
    name: &str,
    created: &str,
    expires: &str,
    expected: Option<&str>,
    last_seen: Option<&str>,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("claim_id", format!("claim-{name}"))
        .unwrap();
    kwargs.set_item("owner", name).unwrap();
    kwargs.set_item("paths", ("p.py",)).unwrap();
    kwargs.set_item("justification", "j").unwrap();
    kwargs.set_item("created_at", created).unwrap();
    kwargs.set_item("expires_at", expires).unwrap();
    if let Some(expected) = expected {
        kwargs.set_item("expected_at", expected).unwrap();
    }
    if let Some(last_seen) = last_seen {
        kwargs.set_item("last_seen", last_seen).unwrap();
    }
    module(py, "conductor.candidate_review.ownership")
        .getattr("OwnershipClaim")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn compact<'py>(py: Python<'py>, claims: Vec<Bound<'py, PyAny>>) -> String {
    let kwargs = PyDict::new(py);
    kwargs.set_item("now", fixed_now(py)).unwrap();
    let output = module(py, "conductor.candidate_review.cli")
        .getattr("compact_claims_text")
        .unwrap()
        .call(
            (PyList::new(py, claims).unwrap(), "abcdef0123456789"),
            Some(&kwargs),
        )
        .unwrap();
    text(&output)
}

#[test]
fn compact_text_counts_expired_and_hides_them() {
    let _case = isolated_case();
    Python::attach(|py| {
        let text = compact(
            py,
            vec![
                claim(
                    py,
                    "live",
                    "2026-08-27T12:00:00+00:00",
                    "2026-08-27T14:00:00+00:00",
                    None,
                    None,
                ),
                claim(
                    py,
                    "dead",
                    "2026-08-27T12:00:00+00:00",
                    "2026-08-27T10:00:00+00:00",
                    None,
                    None,
                ),
            ],
        );
        assert_eq!(
            text.lines().next().unwrap(),
            "claims: 1 active (0 overrun), 1 expired, sha256 abcdef012345"
        );
        assert!(text.contains("claim-live") && !text.contains("claim-dead"));
        assert!(text.contains("due 08-27 14:00Z ends 08-27 12:45Z"));
        assert!(text.contains("on-time"));
        assert!(text.contains("idle   0/45m"));
    });
}

#[test]
fn compact_text_marks_an_overrun_claim() {
    let _case = isolated_case();
    Python::attach(|py| {
        let text = compact(
            py,
            vec![claim(
                py,
                "late",
                "2026-08-27T11:00:00+00:00",
                "2026-08-27T13:00:00+00:00",
                Some("2026-08-27T11:30:00+00:00"),
                Some("2026-08-27T11:55:00+00:00"),
            )],
        );
        assert!(text
            .lines()
            .next()
            .unwrap()
            .starts_with("claims: 1 active (1 overrun),"));
        assert!(text.contains("OVERRUN"));
        assert!(text.contains("idle   5/10m"));
    });
}
