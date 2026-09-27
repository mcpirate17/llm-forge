#![cfg(feature = "python-compat-tests")]
//! Rust-owned candidate review CLI, receipt, and ownership contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{attr_text, module, path, text, AttrPatch, Case};

const POLICY: &str = r#"schema_version = 1
block_at = "high"
max_workers = 1
cache_ttl_days = 1
claim_max_age_hours = 1
max_file_bytes = 1000000
max_binary_bytes = 1000000
coverage_threshold = 75.0
high_risk_coverage_threshold = 90.0
baseline_expires = 2099-01-01
exceptions = []

[classes]

[risk]
high = []

[paths]
protected_deletes = []
hot = []
generated = []

[checks.candidate-integrity]
kind = "builtin"
profiles = ["fast", "full"]
classes = []
severity = "critical"
always = true
cache = false
run_on_deletions = true
timeout_seconds = 10
memory_mb = 128
max_output_chars = 1000
"#;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn fixture(case: &Case, install_engine: bool) -> PathBuf {
    let repo = case.mkdir("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Candidate Review Test"]);
    git(
        &repo,
        &["config", "user.email", "candidate-review@example.invalid"],
    );
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["config", "core.hooksPath", "/dev/null"]);
    if install_engine {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/conductor/candidate_review");
        let target = repo.join("conductor/candidate_review");
        fs::create_dir_all(&target).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            if entry.path().extension().is_some_and(|ext| ext == "py") {
                fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
            }
        }
        fs::write(repo.join("conductor/candidate_policy.toml"), POLICY).unwrap();
    }
    fs::write(repo.join("probe.txt"), "baseline\n").unwrap();
    git(&repo, &["add", "--all"]);
    git(&repo, &["commit", "-qm", "baseline\n\nAgent: llm-fixture"]);
    fs::write(repo.join("probe.txt"), "candidate\n").unwrap();
    git(&repo, &["add", "probe.txt"]);
    repo
}

fn cli<'py>(py: Python<'py>, args: &[String]) -> i64 {
    module(py, "conductor.candidate_review.cli")
        .getattr("main")
        .unwrap()
        .call1((args,))
        .unwrap()
        .extract()
        .unwrap()
}

fn assert_review_receipt_and_attestation(py: Python<'_>, case: &Case, repo: &Path) {
    let repo_name = repo.to_str().unwrap().to_owned();
    let json_out = repo.join("review.json");
    let code = cli(
        py,
        &[
            "review".into(),
            "--repo".into(),
            repo_name.clone(),
            "--surface".into(),
            "pre-commit".into(),
            "--candidate".into(),
            "index".into(),
            "--profile".into(),
            "fast".into(),
            "--json-out".into(),
            json_out.to_str().unwrap().into(),
        ],
    );
    assert_eq!(
        code,
        0,
        "{}",
        fs::read_to_string(&json_out).unwrap_or_default()
    );
    let payload: serde_json::Value = serde_json::from_slice(&fs::read(&json_out).unwrap()).unwrap();
    assert_eq!(payload["decision"], "pass");
    assert!(!payload["receipt_id"].as_str().unwrap().is_empty());
    assert_eq!(
        cli(
            py,
            &["verify-receipt".into(), json_out.to_str().unwrap().into()]
        ),
        0
    );
    let message = case.write("repo/COMMIT_EDITMSG", "test: governed candidate\n");
    assert_eq!(
        cli(
            py,
            &[
                "attest-message".into(),
                message.to_str().unwrap().into(),
                "--repo".into(),
                repo_name.clone()
            ]
        ),
        0
    );
    assert!(fs::read_to_string(&message)
        .unwrap()
        .contains("Governance-Tree:"));
}

#[test]
fn review_attest_claim_and_release_protocol_is_bound_to_the_receipt() {
    let case = Case::new();
    let repo = fixture(&case, true);
    let repo_name = repo.to_str().unwrap().to_owned();
    Python::attach(|py| {
        assert_review_receipt_and_attestation(py, &case, &repo);
        assert_eq!(
            cli(
                py,
                &[
                    "claim".into(),
                    "--repo".into(),
                    repo_name.clone(),
                    "--justification".into(),
                    "exercise the complete CLI ownership protocol".into(),
                    "probe.txt".into()
                ]
            ),
            0
        );
        let ownership = module(py, "conductor.candidate_review.ownership");
        let claims = ownership
            .getattr("load_claims")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        let claim = claims.get_item(0).unwrap().get_item(0).unwrap();
        assert_eq!(
            attr_text(&claim, "owner"),
            text(
                &module(py, "conductor.candidate_review.identity")
                    .getattr("resolve_owner")
                    .unwrap()
                    .call1((path(py, &repo),))
                    .unwrap()
            )
        );
        assert_eq!(
            cli(py, &["claims".into(), "--repo".into(), repo_name.clone()]),
            0
        );
        assert_eq!(
            cli(
                py,
                &[
                    "release-claim".into(),
                    attr_text(&claim, "claim_id"),
                    "--owner".into(),
                    attr_text(&claim, "owner"),
                    "--repo".into(),
                    repo_name.clone()
                ]
            ),
            0
        );
        assert_eq!(
            ownership
                .getattr("load_claims")
                .unwrap()
                .call1((path(py, &repo),))
                .unwrap()
                .get_item(0)
                .unwrap()
                .len()
                .unwrap(),
            0
        );
    });
}

fn assert_failed_review_is_sealed(py: Python<'_>, case: &Case, repo: &Path) {
    let repo_name = repo.to_str().unwrap().to_owned();
    let failed = case.root().join("failed-review.json");
    assert_eq!(
        cli(
            py,
            &[
                "review".into(),
                "--repo".into(),
                repo_name.clone(),
                "--surface".into(),
                "pre-commit".into(),
                "--candidate".into(),
                "index".into(),
                "--profile".into(),
                "fast".into(),
                "--policy".into(),
                "../outside.toml".into(),
                "--json-out".into(),
                failed.to_str().unwrap().into()
            ]
        ),
        2
    );
    let payload: serde_json::Value = serde_json::from_slice(&fs::read(&failed).unwrap()).unwrap();
    assert_eq!(payload["decision"], "fail");
    let json = PyModule::import(py, "json").unwrap();
    let py_payload = json
        .getattr("loads")
        .unwrap()
        .call1((fs::read_to_string(&failed).unwrap(),))
        .unwrap();
    let verified = module(py, "conductor.candidate_review.engine")
        .getattr("verify_receipt_payload")
        .unwrap()
        .call1((py_payload,))
        .unwrap();
    assert_eq!(text(&verified.get_item(0).unwrap()), "True");
}

#[test]
fn cli_failure_paths_write_a_sealed_failure_and_refuse_unbound_inputs() {
    let case = Case::new();
    let repo = fixture(&case, false);
    let repo_name = repo.to_str().unwrap().to_owned();
    Python::attach(|py| {
        assert_failed_review_is_sealed(py, &case, &repo);
        let malformed = case.write("malformed.json", "{");
        assert_eq!(
            cli(
                py,
                &["verify-receipt".into(), malformed.to_str().unwrap().into()]
            ),
            1
        );
        let receipt = module(py, "conductor.test_candidate_review")
            .getattr("_receipt")
            .unwrap()
            .call0()
            .unwrap();
        let unbound = case.root().join("unbound-receipt.json");
        module(py, "conductor.candidate_review.model")
            .getattr("write_json_atomic")
            .unwrap()
            .call1((path(py, &unbound), receipt.call_method0("to_dict").unwrap()))
            .unwrap();
        assert_eq!(
            cli(
                py,
                &[
                    "verify-receipt".into(),
                    unbound.to_str().unwrap().into(),
                    "--repo".into(),
                    repo_name.clone(),
                    "--ref".into(),
                    "HEAD".into()
                ]
            ),
            1
        );
        assert_eq!(
            cli(
                py,
                &[
                    "attest-message".into(),
                    case.root().join("missing-message").to_str().unwrap().into(),
                    "--repo".into(),
                    repo_name.clone()
                ]
            ),
            1
        );
        assert_eq!(
            cli(
                py,
                &[
                    "release-claim".into(),
                    "claim-does-not-exist".into(),
                    "--owner".into(),
                    "Codex".into(),
                    "--repo".into(),
                    repo_name.clone()
                ]
            ),
            1
        );
        let ownership = module(py, "conductor.candidate_review.ownership");
        let store = ownership
            .getattr("claim_store_path")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        let store_path = PathBuf::from(text(&store));
        fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        fs::write(store_path, "not-json").unwrap();
        assert_eq!(
            cli(py, &["claims".into(), "--repo".into(), repo_name.clone()]),
            1
        );
    });
}

#[test]
fn fix_command_preserves_subprocess_exit_and_requires_paths() {
    let case = Case::new();
    let repo = fixture(&case, false);
    Python::attach(|py| {
        let cli_module = module(py, "conductor.candidate_review.cli");
        let args = cli_module
            .getattr("_parser")
            .unwrap()
            .call0()
            .unwrap()
            .call_method1(
                "parse_args",
                (vec!["fix", "--repo", repo.to_str().unwrap(), "probe.txt"],),
            )
            .unwrap();
        let subprocess = PyModule::import(py, "subprocess").unwrap();
        let completed = subprocess
            .getattr("CompletedProcess")
            .unwrap()
            .call1((Vec::<String>::new(), 7))
            .unwrap();
        let mock = PyModule::import(py, "unittest.mock")
            .unwrap()
            .getattr("Mock")
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("return_value", completed).unwrap();
        let fake_run = mock.call((), Some(&kwargs)).unwrap();
        let _patch = AttrPatch::replace(&subprocess, "run", &fake_run);
        let root_kwargs = PyDict::new(py);
        root_kwargs
            .set_item("return_value", path(py, &repo))
            .unwrap();
        let root = mock.call((), Some(&root_kwargs)).unwrap();
        let _root = AttrPatch::replace(&cli_module, "repository_root", &root);
        assert_eq!(
            cli_module
                .getattr("fix_command")
                .unwrap()
                .call1((&args,))
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            7
        );
        args.setattr("paths", Vec::<String>::new()).unwrap();
        assert_eq!(
            cli_module
                .getattr("fix_command")
                .unwrap()
                .call1((args,))
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            2
        );
    });
}

fn command_check_context<'py>(
    py: Python<'py>,
    case: &Case,
    repo: &Path,
) -> (Bound<'py, PyAny>, Bound<'py, PyAny>) {
    let policy = module(py, "conductor.candidate_review.policy")
        .getattr("load_policy")
        .unwrap()
        .call1((path(py, &repo.join("conductor/candidate_policy.toml")),))
        .unwrap();
    let resolve = module(py, "conductor.candidate_review.git_source")
        .getattr("resolve_candidate")
        .unwrap();
    let candidate_kwargs = PyDict::new(py);
    candidate_kwargs.set_item("kind", "index").unwrap();
    let candidate = resolve
        .call((path(py, repo),), Some(&candidate_kwargs))
        .unwrap();
    let check = policy.getattr("checks").unwrap().get_item(0).unwrap();
    let changed = PyDict::new(py);
    changed.set_item("kind", "command").unwrap();
    changed.set_item("command", ("probe",)).unwrap();
    changed.set_item("version_command", ("probe",)).unwrap();
    changed.set_item("always", true).unwrap();
    let check = PyModule::import(py, "dataclasses")
        .unwrap()
        .getattr("replace")
        .unwrap()
        .call((&check,), Some(&changed))
        .unwrap();
    let ctx_args = PyDict::new(py);
    for (key, value) in [
        ("repo", path(py, repo)),
        ("snapshot", path(py, repo)),
        ("runtime_dir", path(py, &case.root().join("runtime"))),
    ] {
        ctx_args.set_item(key, value).unwrap();
    }
    ctx_args.set_item("candidate", candidate).unwrap();
    ctx_args.set_item("entries", ()).unwrap();
    ctx_args.set_item("policy", policy).unwrap();
    ctx_args.set_item("surface", "pre-commit").unwrap();
    ctx_args.set_item("profile", "fast").unwrap();
    ctx_args.set_item("owner", py.None()).unwrap();
    let ctx = module(py, "conductor.candidate_review.checks")
        .getattr("ReviewContext")
        .unwrap()
        .call((), Some(&ctx_args))
        .unwrap();
    (ctx, check)
}

#[test]
fn command_analyzer_errors_block_and_resource_limits_are_bounded() {
    let case = Case::new();
    let repo = fixture(&case, true);
    Python::attach(|py| {
        let runner = module(py, "conductor.candidate_review.command_runner");
        let mock_class = PyModule::import(py, "unittest.mock")
            .unwrap()
            .getattr("Mock")
            .unwrap();
        let mock_return = |value: &Bound<'_, PyAny>| {
            let kwargs = PyDict::new(py);
            kwargs.set_item("return_value", value).unwrap();
            mock_class.call((), Some(&kwargs)).unwrap()
        };
        let which = mock_return(&pyo3::types::PyString::new(py, "prlimit").into_any());
        let _which = AttrPatch::replace(&runner.getattr("shutil").unwrap(), "which", &which);
        let limits = PyDict::new(py);
        limits
            .set_item("side_effect", vec![(0, 1024), (0, 2)])
            .unwrap();
        let getrlimit = mock_class.call((), Some(&limits)).unwrap();
        let _limit = AttrPatch::replace(
            &runner.getattr("resource").unwrap(),
            "getrlimit",
            &getrlimit,
        );
        let kwargs = PyDict::new(py);
        kwargs.set_item("memory_mb", 128).unwrap();
        kwargs.set_item("timeout_seconds", 10).unwrap();
        let limited = runner
            .getattr("_limited_command")
            .unwrap()
            .call((vec!["probe"],), Some(&kwargs))
            .unwrap();
        assert_eq!(text(&limited.get_item(0).unwrap()), "prlimit");
        assert_eq!(text(&limited.get_item(1).unwrap()), "--as=1024");
        assert_eq!(text(&limited.get_item(2).unwrap()), "--cpu=2");

        let (ctx, check) = command_check_context(py, &case, &repo);
        let subprocess = PyModule::import(py, "subprocess").unwrap();
        let failed = subprocess
            .getattr("CompletedProcess")
            .unwrap()
            .call1((Vec::<String>::new(), 7, "", "version failure"))
            .unwrap();
        let fake_run = mock_return(&failed);
        let _run = AttrPatch::replace(&runner, "_run_process", &fake_run);
        let version = runner
            .getattr("tool_version")
            .unwrap()
            .call1((&ctx, &check))
            .unwrap();
        assert_eq!(
            text(&version.get_item(1).unwrap()),
            "exit 7: version failure"
        );
        for (error, expected) in [
            (
                subprocess
                    .getattr("TimeoutExpired")
                    .unwrap()
                    .call1((vec!["probe"], 1))
                    .unwrap(),
                "analyzer-timeout",
            ),
            (
                PyModule::import(py, "builtins")
                    .unwrap()
                    .getattr("OSError")
                    .unwrap()
                    .call1(("deliberate analyzer crash",))
                    .unwrap(),
                "analyzer-crash",
            ),
        ] {
            let side = PyDict::new(py);
            side.set_item("side_effect", error).unwrap();
            let raising = mock_class.call((), Some(&side)).unwrap();
            let _patch = AttrPatch::replace(&runner, "_run_process", &raising);
            let kwargs = PyDict::new(py);
            kwargs.set_item("version", "1").unwrap();
            let result = runner
                .getattr("run_command_check")
                .unwrap()
                .call((&ctx, &check), Some(&kwargs))
                .unwrap();
            assert_eq!(
                attr_text(
                    &result.getattr("findings").unwrap().get_item(0).unwrap(),
                    "rule_id"
                ),
                expected
            );
        }
    });
}
