#![cfg(feature = "python-compat-tests")]
//! Candidate-review runtime and changed-line contracts, source cases 1–9 of 17.

#[path = "python_contracts/candidate_review_support.rs"]
#[allow(dead_code)]
mod candidate_review_support;
#[path = "python_contracts/candidate_runtime_support.rs"]
#[allow(dead_code)]
mod candidate_runtime_support;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use candidate_review_support as fixture;
use candidate_runtime_support as runtime;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PySet};
use serde_json::json;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use support::{module, path, AttrPatch};

fn call_bool<'py>(object: &Bound<'py, PyAny>, name: &str, arg: impl IntoPyObject<'py>) -> bool {
    object
        .call_method1(name, (arg,))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn locked_commit_reuses_mutex_in_precommit_hook() {
    let case = fixture::isolated_case();
    let repo = runtime::repo(&case);
    runtime::write(&repo, "probe.txt", "baseline\n");
    fixture::commit_all(&repo, "baseline");
    fixture::git(&repo, &["config", "--unset", "core.hooksPath"]);
    let hook = repo.join(".git/hooks/pre-commit");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("src")
        .canonicalize()
        .unwrap();
    let interpreter = Python::attach(|py| {
        module(py, "sys")
            .getattr("executable")
            .unwrap()
            .extract::<String>()
            .unwrap()
    });
    fs::write(
        &hook,
        format!(
            "#!{interpreter}\nimport os\nimport sys\nfrom pathlib import Path\nsys.path.insert(0, {source:?})\nfrom conductor.candidate_review.engine import governance_lock\ninherited_fd = os.environ.pop('LLM_GOVERNANCE_COMMIT_LOCK_FD', None)\nif inherited_fd is not None:\n    try:\n        os.close(int(inherited_fd))\n    except OSError:\n        pass\nwith governance_lock(Path.cwd(), exclusive=False, timeout_seconds=0.2):\n    pass\n",
            source = source.to_str().unwrap()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    runtime::write(&repo, "probe.txt", "candidate\n");
    fixture::git(&repo, &["add", "probe.txt"]);
    Python::attach(|py| {
        let rc: i32 = module(py, "conductor.candidate_review.engine")
            .getattr("run_locked_git_commit")
            .unwrap()
            .call1((path(py, &repo), vec!["commit", "-m", "candidate"]))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(rc, 0);
    });
    assert_eq!(
        fixture::git(&repo, &["show", "HEAD:probe.txt"]),
        "candidate"
    );
}

#[test]
fn inherited_lock_descriptor_validation() {
    let mut case = fixture::isolated_case();
    let repo = runtime::repo(&case);
    Python::attach(|py| {
        let engine = module(py, "conductor.candidate_review.engine");
        let lock: std::path::PathBuf = engine
            .getattr("_governance_lock_path")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap()
            .extract()
            .unwrap();
        assert!(call_bool(
            engine.as_any(),
            "_is_ancestor_process",
            module(py, "os").call_method0("getppid").unwrap()
        ));
        assert!(!call_bool(engine.as_any(), "_is_ancestor_process", -1));
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        fs::write(&lock, "").unwrap();
        let fd_env: String = engine
            .getattr("INHERITED_LOCK_FD_ENV")
            .unwrap()
            .extract()
            .unwrap();
        // The env name is a static production constant; Case restores it after the test.
        assert_eq!(fd_env, "LLM_GOVERNANCE_COMMIT_LOCK_FD");
        case.remove_env("LLM_GOVERNANCE_COMMIT_LOCK_FD");
        let inherited = engine.getattr("_inherited_lock_fd").unwrap();
        assert!(inherited.call1((path(py, &lock),)).unwrap().is_none());
        for invalid in ["not-an-integer", "1"] {
            case.set_env("LLM_GOVERNANCE_COMMIT_LOCK_FD", invalid);
            assert!(inherited.call1((path(py, &lock),)).unwrap().is_none());
        }
        let unrelated = fs::File::create(case.root().join("unrelated.lock")).unwrap();
        case.set_env(
            "LLM_GOVERNANCE_COMMIT_LOCK_FD",
            &unrelated.as_raw_fd().to_string(),
        );
        assert!(inherited.call1((path(py, &lock),)).unwrap().is_none());
        let matching = fs::OpenOptions::new().append(true).open(&lock).unwrap();
        case.set_env(
            "LLM_GOVERNANCE_COMMIT_LOCK_FD",
            &matching.as_raw_fd().to_string(),
        );
        let actual: i32 = inherited
            .call1((path(py, &lock),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(actual, matching.as_raw_fd());
        drop(matching);
        case.set_env("LLM_GOVERNANCE_COMMIT_LOCK_TOKEN", "short");
        let valid = engine.getattr("_inherited_lock_token_valid").unwrap();
        assert!(!valid
            .call1((path(py, &lock),))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let token = "a".repeat(64);
        case.set_env("LLM_GOVERNANCE_COMMIT_LOCK_TOKEN", &token);
        fs::write(&lock, "not json\n").unwrap();
        assert!(!valid
            .call1((path(py, &lock),))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        fs::write(
            &lock,
            json!({"pid": std::process::id(), "token": "b".repeat(64)}).to_string(),
        )
        .unwrap();
        assert!(!valid
            .call1((path(py, &lock),))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let kwargs = PyDict::new(py);
        kwargs.set_item("exclusive", true).unwrap();
        kwargs.set_item("timeout_seconds", 1.0).unwrap();
        kwargs.set_item("lease_token", &token).unwrap();
        let context = engine
            .getattr("_held_governance_lock")
            .unwrap()
            .call((path(py, &repo),), Some(&kwargs))
            .unwrap();
        context.call_method0("__enter__").unwrap();
        assert!(valid
            .call1((path(py, &lock),))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        context
            .call_method1("__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
    });
}

#[test]
fn shard_thread_environment_gives_each_worker_a_fair_share() {
    let _case = fixture::isolated_case();
    Python::attach(|py| {
        let sharding = module(py, "conductor.candidate_review.sharding");
        let os = sharding.getattr("os").unwrap();
        let four =
            PyCFunction::new_closure(py, None, None, |_, _| -> PyResult<usize> { Ok(4) }).unwrap();
        let _patch = AttrPatch::replace(&os, "cpu_count", four.as_any());
        let environment = sharding.getattr("shard_thread_environment").unwrap();
        let pins: Vec<String> = sharding
            .getattr("_THREAD_PIN_VARIABLES")
            .unwrap()
            .extract()
            .unwrap();
        let actual = runtime::json_value(&environment.call1((4,)).unwrap());
        assert_eq!(actual.as_object().unwrap().len(), pins.len());
        for pin in &pins {
            assert_eq!(actual[pin], "1");
        }
        assert_eq!(
            runtime::json_value(&environment.call1((1,)).unwrap())["OMP_NUM_THREADS"],
            "4"
        );
        assert_eq!(
            runtime::json_value(&environment.call1((16,)).unwrap())["OMP_NUM_THREADS"],
            "1"
        );
        let none = PyCFunction::new_closure(py, None, None, |args, _| -> PyResult<Py<PyAny>> {
            Ok(args.py().None())
        })
        .unwrap();
        let _second = AttrPatch::replace(&os, "cpu_count", none.as_any());
        assert_eq!(
            runtime::json_value(&environment.call1((4,)).unwrap())["MKL_NUM_THREADS"],
            "1"
        );
    });
}

#[test]
fn wall_budget_is_separable_from_the_cpu_budget() {
    let _case = fixture::isolated_case();
    Python::attach(|py| {
        let policy = runtime::policy(py);
        let checks = policy.getattr("checks").unwrap();
        let full = checks
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .find(|row| {
                row.getattr("check_id")
                    .unwrap()
                    .extract::<String>()
                    .unwrap()
                    == "targeted-tests-full"
            })
            .unwrap();
        let cpu: f64 = full.getattr("timeout_seconds").unwrap().extract().unwrap();
        let wall: f64 = full
            .getattr("wall_timeout_seconds")
            .unwrap()
            .extract()
            .unwrap();
        assert!(wall > cpu);
        let zero = 0_i32.into_pyobject(py).unwrap().into_any();
        let default = runtime::replace(py, &full, &[("wall_timeout_override", &zero)]);
        assert_eq!(
            default
                .getattr("wall_timeout_seconds")
                .unwrap()
                .extract::<f64>()
                .unwrap(),
            cpu
        );
    });
}

fn sharded_policy<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let policy = runtime::policy(py);
    let full = policy
        .getattr("checks")
        .unwrap()
        .try_iter()
        .unwrap()
        .map(Result::unwrap)
        .find(|row| {
            row.getattr("check_id")
                .unwrap()
                .extract::<String>()
                .unwrap()
                == "targeted-tests-full"
        })
        .unwrap();
    let two = 2_i32.into_pyobject(py).unwrap().into_any();
    let four = 4_i32.into_pyobject(py).unwrap().into_any();
    runtime::replace(
        py,
        &full,
        &[("shard_max_files", &two), ("shard_workers", &four)],
    )
}

fn stalled_shards(all: bool) -> (Vec<String>, serde_json::Value, Vec<String>) {
    let case = fixture::isolated_case();
    let repo = runtime::staged_probe(&case);
    Python::attach(|py| {
        let sharded = sharded_policy(py);
        let candidate = runtime::staged_candidate(py, &repo);
        let tests: Vec<_> = (0..8).map(|i| format!("t{i}.py")).collect();
        let tests: Vec<_> = tests.iter().map(String::as_str).collect();
        let wide = fixture::test_selection(py, &tests);
        let sharding = module(py, "conductor.candidate_review.sharding");
        let wall: f64 = sharded
            .getattr("wall_timeout_seconds")
            .unwrap()
            .extract()
            .unwrap();
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
                let command = args.get_item(0)?;
                let stall = all || command.contains("t0.py")?;
                let subprocess = module(args.py(), "subprocess");
                if stall {
                    let error = subprocess
                        .getattr("TimeoutExpired")?
                        .call1((command, wall))?;
                    return Err(PyErr::from_value(error));
                }
                Ok(subprocess
                    .getattr("CompletedProcess")?
                    .call1((command, 1, "..F [ 50%]", ""))?
                    .unbind())
            })
            .unwrap();
        let _patch = AttrPatch::replace(sharding.as_any(), "_run_process", callback.as_any());
        runtime::with_context(
            py,
            &repo,
            &candidate,
            "pre-commit",
            "full",
            &case.root().join("runtime"),
            |ctx| {
                let kwargs = PyDict::new(py);
                kwargs.set_item("coverage", false).unwrap();
                let result = module(py, "conductor.candidate_review.verification")
                    .getattr("run_targeted_tests")
                    .unwrap()
                    .call((ctx, wide, sharded.clone()), Some(&kwargs))
                    .unwrap();
                let findings = result
                    .getattr("findings")
                    .unwrap()
                    .try_iter()
                    .unwrap()
                    .map(|row| {
                        row.unwrap()
                            .getattr("rule_id")
                            .unwrap()
                            .extract::<String>()
                            .unwrap()
                    })
                    .collect::<Vec<_>>();
                let evidence = runtime::json_value(
                    &result
                        .getattr("findings")
                        .unwrap()
                        .get_item(0)
                        .unwrap()
                        .getattr("evidence")
                        .unwrap(),
                );
                let command = result
                    .getattr("command")
                    .unwrap()
                    .extract::<Vec<String>>()
                    .unwrap();
                (findings, evidence, command)
            },
        )
    })
}

#[test]
fn a_stalled_shard_does_not_discard_the_other_shards_results() {
    let (rules, evidence, command) = stalled_shards(false);
    assert_eq!(rules[0], "targeted-test-timeout");
    assert!(rules.contains(&"targeted-test-failure".to_owned()));
    assert_eq!(evidence["timed_out_shards"], json!([1]));
    assert_eq!(evidence["shard_count"], 4);
    assert!(evidence["wall_budget_seconds"].as_f64().unwrap() > 0.0);
    assert!(evidence["cpu_budget_seconds"].as_f64().unwrap() > 0.0);
    assert!(!command.contains(&"t0.py".to_owned()));
}

#[test]
fn every_stalled_shard_still_fails_the_check() {
    let (rules, evidence, _) = stalled_shards(true);
    assert_eq!(rules, ["targeted-test-timeout"]);
    assert_eq!(evidence["timed_out_shards"], json!([1, 2, 3, 4]));
}

#[test]
fn changed_coverage_judges_each_risk_class_on_its_own_lines() {
    let _case = fixture::isolated_case();
    Python::attach(|py| {
        let per_file = json!({
            "conductor/agent_a2a.py": {"covered": 95, "measurable": 100},
            "research/synthesis/wavelet.py": {"covered": 80, "measurable": 100}
        });
        let risk =
            json!({"conductor/agent_a2a.py": "high", "research/synthesis/wavelet.py": "low"});
        let loads = module(py, "json").getattr("loads").unwrap();
        let files = loads.call1((per_file.to_string(),)).unwrap();
        let risk = loads.call1((risk.to_string(),)).unwrap();
        let buckets = module(py, "conductor.candidate_review.verification")
            .getattr("_risk_buckets")
            .unwrap();
        let result = runtime::json_value(&buckets.call1((&files, risk)).unwrap());
        assert_eq!(result["high"], json!({"covered": 95, "measurable": 100}));
        assert_eq!(result["other"], json!({"covered": 80, "measurable": 100}));
        let covered = result["high"]["covered"].as_u64().unwrap()
            + result["other"]["covered"].as_u64().unwrap();
        let measurable = result["high"]["measurable"].as_u64().unwrap()
            + result["other"]["measurable"].as_u64().unwrap();
        assert!(covered * 100 < measurable * 90);
        let empty = PyDict::new(py);
        let result = runtime::json_value(&buckets.call1((files, empty)).unwrap());
        assert_eq!(result["other"]["measurable"], 200);
        assert_eq!(result["high"]["measurable"], 0);
    });
}

#[test]
fn changed_lines_score_a_move_by_its_edited_hunks_only() {
    let case = fixture::isolated_case();
    let repo = runtime::repo(&case);
    for (name, prefix) in [
        ("pure.py", "PURE"),
        ("edited.py", "EDIT"),
        ("touched.py", "TOUCH"),
    ] {
        runtime::write(&repo, name, &runtime::numbered(prefix, 8));
    }
    fixture::commit_all(&repo, "baseline");
    fixture::git(&repo, &["mv", "pure.py", "moved_pure.py"]);
    fixture::git(&repo, &["mv", "edited.py", "moved_edited.py"]);
    runtime::write(
        &repo,
        "moved_edited.py",
        &runtime::numbered("EDIT", 8).replace("EDIT_4 = 4\n", "EDIT_4 = 40\n"),
    );
    runtime::write(&repo, "added.py", &runtime::numbered("ADD", 4));
    runtime::write(
        &repo,
        "touched.py",
        &runtime::numbered("TOUCH", 8).replace("TOUCH_1 = 1\n", "TOUCH_1 = 11\n"),
    );
    fixture::git(&repo, &["add", "--all"]);
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "index", None, None);
        let paths: Vec<String> = candidate
            .getattr("changes")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|row| row.unwrap().getattr("path").unwrap().extract().unwrap())
            .collect();
        let expected = ["moved_pure.py", "moved_edited.py", "added.py", "touched.py"];
        assert_eq!(
            paths
                .iter()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            expected.into_iter().collect()
        );
        let changed = module(py, "conductor.candidate_review.git_source")
            .getattr("changed_line_numbers")
            .unwrap()
            .call1((path(py, &repo), candidate, paths))
            .unwrap();
        let pure = changed
            .call_method1("get", ("moved_pure.py", PySet::empty(py).unwrap()))
            .unwrap();
        assert_eq!(pure.len().unwrap(), 0);
        assert!(!changed.contains("pure.py").unwrap());
        for (name, lines) in [
            ("moved_edited.py", vec![4]),
            ("added.py", vec![1, 2, 3, 4]),
            ("touched.py", vec![1]),
        ] {
            let actual: std::collections::BTreeSet<i32> =
                changed.get_item(name).unwrap().extract().unwrap();
            assert_eq!(actual, lines.into_iter().collect());
        }
    });
}

#[test]
fn changed_lines_pair_a_move_that_leaves_an_alias_shim() {
    let case = fixture::isolated_case();
    let repo = runtime::repo(&case);
    runtime::write(&repo, "tools/mod.py", &runtime::numbered("LINE", 40));
    fixture::commit_all(&repo, "baseline");
    runtime::write(
        &repo,
        "pkg/mod.py",
        &runtime::numbered("LINE", 40).replace("LINE_10 = 10\n", "LINE_10 = 100\n"),
    );
    runtime::write(
        &repo,
        "tools/mod.py",
        "from pkg.mod import LINE_1  # moved\n",
    );
    fixture::git(&repo, &["add", "--all"]);
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "index", None, None);
        let changed = module(py, "conductor.candidate_review.git_source")
            .getattr("changed_line_numbers")
            .unwrap()
            .call1((path(py, &repo), candidate, ["pkg/mod.py", "tools/mod.py"]))
            .unwrap();
        let moved: std::collections::BTreeSet<i32> =
            changed.get_item("pkg/mod.py").unwrap().extract().unwrap();
        let shim: std::collections::BTreeSet<i32> =
            changed.get_item("tools/mod.py").unwrap().extract().unwrap();
        assert_eq!(moved, [10].into());
        assert_eq!(shim, [1].into());
    });
}
