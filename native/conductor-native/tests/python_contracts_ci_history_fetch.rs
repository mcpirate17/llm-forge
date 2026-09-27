#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped CI-history cache fetcher.

#[path = "python_contracts/audit_fixture.rs"]
#[allow(dead_code)]
mod audit_fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use audit_fixture::{capture, captured, git, isolated_case, json_value, patch_static, py_json};
use pyo3::exceptions::PyAssertionError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use support::{module, path, text, AttrPatch, Case};

fn fetcher<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.ci_history_fetch")
}

fn rust_fixture() -> Value {
    json!({
        "fetched_utc": "2026-09-13T00:00:00Z",
        "prs": {
            "50": {"branch":"forge/routing-policy","first_push_sha":"aaa111","first_push_ci":"red",
                "commits":[
                    {"sha":"aaa111","subject":"feat: first cut","trailers":{"Agent":"glm"}},
                    {"sha":"bbb222","subject":"fix: CI","trailers":{"Agent":"llm-b0","Claude-Session":"https://claude.ai/code/session_01X"}}
                ]},
            "48": {"branch":"forge/ledger-join","first_push_sha":"ccc333","first_push_ci":"green",
                "commits":[{"sha":"ccc333","subject":"feat: clean landing","trailers":{"Agent":"llm-b0"}}]}
        }
    })
}

fn recorded_list() -> Value {
    json!([
        {"number":50,"headRefName":"forge/routing-policy","mergedAt":"2026-09-12T10:00:00Z"},
        {"number":51,"headRefName":"forge/bytecode","mergedAt":"2026-09-12T11:00:00Z"}
    ])
}

fn check_runs(kind: &str) -> Value {
    match kind {
        "red" => json!({"total_count":2,"check_runs":[
            {"status":"completed","conclusion":"success"},
            {"status":"completed","conclusion":"failure"}]}),
        "green" => json!({"total_count":2,"check_runs":[
            {"status":"completed","conclusion":"success"},
            {"status":"completed","conclusion":"skipped"}]}),
        "unknown" => json!({"total_count":2,"check_runs":[
            {"status":"in_progress","conclusion":null},
            {"status":"queued","conclusion":null}]}),
        _ => panic!("unknown fixture"),
    }
}

fn fixture_repo(case: &Case) -> (PathBuf, Vec<String>) {
    let origin = case.root().join("origin.git");
    git(
        case.root(),
        &["init", "-q", "--bare", origin.to_str().unwrap()],
    );
    let work = case.mkdir("work");
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.name", "t"]);
    git(&work, &["config", "user.email", "t@t"]);
    let mut shas = Vec::new();
    for (message, pr) in [
        ("feat: first cut\n\nLong body.\n\nAgent: glm\n", None),
        ("fix: CI\n\nAgent: llm-b0\nClaude-Session: https://claude.ai/code/session_01X\nCo-Authored-By: Someone <s@example.com>\n", Some("50")),
        ("feat: newer thing\n\nAgent: glm\n", Some("51")),
    ] {
        fs::write(work.join("f.txt"), message).unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", message]);
        shas.push(git(&work, &["rev-parse", "HEAD"]).trim().to_owned());
        if let Some(pr) = pr {
            git(&work, &["push", "-q", origin.to_str().unwrap(), &format!("HEAD:refs/pull/{pr}/head")]);
        }
    }
    let clone = case.root().join("clone");
    git(
        case.root(),
        &[
            "clone",
            "-q",
            "--origin",
            "origin",
            origin.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    git(&clone, &["config", "user.name", "t"]);
    git(&clone, &["config", "user.email", "t@t"]);
    (clone, shas)
}

fn github_origin(clone: &Path) {
    git(
        clone,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/octo/widget.git",
        ],
    );
}

fn view_50(shas: &[String]) -> Value {
    json!({"number":50,"headRefName":"forge/routing-policy","commits":[
        {"oid":shas[0],"messageHeadline":"feat: first cut"},
        {"oid":shas[1],"messageHeadline":"fix: CI"}]})
}

fn fake_replies(number: i32, view: Value, sha: &str, checks: Value) -> Value {
    let mut replies = serde_json::Map::new();
    replies.insert(format!("view:{number}"), view);
    replies.insert(format!("checks:{sha}"), checks);
    Value::Object(replies)
}

fn client<'py>(py: Python<'py>, repo: &Path) -> Bound<'py, PyAny> {
    fetcher(py)
        .getattr("GhClient")
        .unwrap()
        .call1((path(py, repo), "octo", "widget"))
        .unwrap()
}

struct FakeGh {
    _patches: Vec<AttrPatch>,
    calls: Arc<Mutex<Vec<String>>>,
}

fn fake_gh(py: Python<'_>, replies: Value, lists: Value, fail_50: bool) -> FakeGh {
    let class = fetcher(py).getattr("GhClient").unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let auth_calls = calls.clone();
    let auth = PyCFunction::new_closure(py, None, None, move |_, _| -> PyResult<()> {
        auth_calls.lock().unwrap().push("auth".into());
        Ok(())
    })
    .unwrap();
    let list_calls = calls.clone();
    let list = PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<Py<PyAny>> {
        list_calls.lock().unwrap().push("list".into());
        let limit = kw
            .and_then(|k| k.get_item("limit").unwrap())
            .and_then(|v| v.extract::<usize>().ok())
            .unwrap_or_else(|| lists.as_array().unwrap().len());
        let rows = lists.as_array().unwrap()[..limit.min(lists.as_array().unwrap().len())].to_vec();
        Ok(py_json(args.py(), &Value::Array(rows)).unbind())
    })
    .unwrap();
    let view_calls = calls.clone();
    let checks_replies = replies.clone();
    let view = PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
        let number: i32 = args.get_item(0)?.extract()?;
        view_calls.lock().unwrap().push(format!("view:{number}"));
        if fail_50 && number == 50 {
            let error = fetcher(args.py()).getattr("GhError")?.call1(("boom",))?;
            return Err(PyErr::from_value(error));
        }
        let response = replies
            .get(format!("view:{number}"))
            .expect("recorded view");
        Ok(py_json(args.py(), response).unbind())
    })
    .unwrap();
    let check_calls = calls.clone();
    let check = PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
        let sha: String = args.get_item(0)?.extract()?;
        check_calls.lock().unwrap().push(format!("checks:{sha}"));
        Ok(py_json(
            args.py(),
            checks_replies
                .get(format!("checks:{sha}"))
                .expect("recorded checks"),
        )
        .unbind())
    })
    .unwrap();
    let patches = vec![
        patch_static(class.as_any(), "require_ready", auth.as_any()),
        patch_static(class.as_any(), "list_merged", list.as_any()),
        patch_static(class.as_any(), "view_pr", view.as_any()),
        patch_static(class.as_any(), "check_runs", check.as_any()),
    ];
    FakeGh {
        _patches: patches,
        calls,
    }
}

fn fetch(py: Python<'_>, gh: &Bound<'_, PyAny>, repo: &Path, out: &Path) -> (i32, Vec<i32>) {
    fetcher(py)
        .getattr("fetch")
        .unwrap()
        .call1((gh, path(py, repo), path(py, out)))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn the_schema_round_trips_the_rust_fixture() {
    let _case = isolated_case();
    Python::attach(|py| {
        let fixture = rust_fixture();
        let class = fetcher(py).getattr("CiHistory").unwrap();
        let history = class
            .call_method1("model_validate", (py_json(py, &fixture),))
            .unwrap();
        let dumped = history.call_method0("dump").unwrap();
        let written: Value = serde_json::from_str(&text(&dumped)).unwrap();
        assert_eq!(
            written.as_object().unwrap().keys().collect::<BTreeSet<_>>(),
            fixture.as_object().unwrap().keys().collect::<BTreeSet<_>>()
        );
        assert_eq!(
            written["prs"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<BTreeSet<_>>(),
            fixture["prs"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<BTreeSet<_>>()
        );
        for (number, row) in fixture["prs"].as_object().unwrap() {
            let item = &written["prs"][number];
            assert_eq!(
                item.as_object().unwrap().keys().collect::<BTreeSet<_>>(),
                row.as_object().unwrap().keys().collect::<BTreeSet<_>>()
            );
            for (index, commit) in row["commits"].as_array().unwrap().iter().enumerate() {
                assert_eq!(
                    item["commits"][index]
                        .as_object()
                        .unwrap()
                        .keys()
                        .collect::<BTreeSet<_>>(),
                    commit.as_object().unwrap().keys().collect::<BTreeSet<_>>()
                );
                assert_eq!(
                    item["commits"][index]["trailers"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .collect::<BTreeSet<_>>(),
                    commit["trailers"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .collect::<BTreeSet<_>>()
                );
            }
        }
        let trailer = fetcher(py).getattr("CiTrailers").unwrap();
        let bare = trailer
            .call_method1("model_validate", (PyDict::new(py),))
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("by_alias", true).unwrap();
        kwargs.set_item("exclude_none", true).unwrap();
        assert!(!bare
            .call_method("model_dump", (), Some(&kwargs))
            .unwrap()
            .contains("Agent")
            .unwrap());
        let agent = trailer
            .call_method1("model_validate", (py_json(py, &json!({"Agent":"glm"})),))
            .unwrap();
        assert!(agent
            .call_method("model_dump", (), Some(&kwargs))
            .unwrap()
            .contains("Agent")
            .unwrap());
    });
}

#[test]
fn first_push_ci_classifies_the_recorded_check_runs() {
    let _case = isolated_case();
    Python::attach(|py| {
        let decide = fetcher(py).getattr("first_push_ci").unwrap();
        for (input, expected) in [
            (check_runs("green"), "green"),
            (check_runs("red"), "red"),
            (check_runs("unknown"), "unknown"),
            (json!({"check_runs":[]}), "unknown"),
        ] {
            assert_eq!(
                text(&decide.call1((py_json(py, &input),)).unwrap()),
                expected
            );
        }
    });
}

#[test]
fn the_merged_listing_always_names_an_explicit_limit() {
    let _case = isolated_case();
    Python::attach(|py| {
        let argvs = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let seen = argvs.clone();
        let callback = PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<&str> {
            seen.lock()
                .unwrap()
                .push(args.iter().map(|a| a.extract().unwrap()).collect());
            Ok("[]")
        })
        .unwrap();
        let class = fetcher(py).getattr("GhClient").unwrap();
        let _patch = patch_static(class.as_any(), "_run_gh", callback.as_any());
        let gh = client(py, Path::new("."));
        let default_limit: usize = fetcher(py)
            .getattr("_DEFAULT_LIMIT")
            .unwrap()
            .extract()
            .unwrap();
        gh.call_method0("list_merged").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("limit", 5).unwrap();
        gh.call_method("list_merged", (), Some(&kwargs)).unwrap();
        let rows = argvs.lock().unwrap();
        assert_eq!(
            rows[0][rows[0].len() - 2..].to_vec(),
            vec!["--limit".to_owned(), default_limit.to_string()]
        );
        assert_eq!(&rows[1][rows[1].len() - 2..], &["--limit", "5"]);
    });
}

#[test]
fn trailers_come_from_interpret_trailers_not_a_regex() {
    let case = isolated_case();
    let message = "feat: a subject\n\nBody line.\n\nAgent: glm\nClaude-Session: https://claude.ai/code/session_1\nSigned-off-by: A U Thor <author@example.com>\n";
    Python::attach(|py| {
        let trailers = fetcher(py).getattr("trailers_of").unwrap();
        let parsed = json_value(
            py,
            &trailers.call1((message, path(py, case.root()))).unwrap(),
        );
        assert_eq!(parsed["Agent"], "glm");
        assert_eq!(parsed["Claude-Session"], "https://claude.ai/code/session_1");
        assert_eq!(
            json_value(
                py,
                &trailers
                    .call1(("no trailers here\n", path(py, case.root())))
                    .unwrap()
            ),
            json!({})
        );
    });
}

#[test]
fn a_full_fetch_reads_the_pr_head_and_writes_the_cache() {
    let case = isolated_case();
    let (clone, shas) = fixture_repo(&case);
    let out = case.root().join("cache.json");
    Python::attach(|py| {
        let gh = client(py, &clone);
        let replies = fake_replies(50, view_50(&shas), &shas[0], check_runs("red"));
        let _fake = fake_gh(py, replies, json!([recorded_list()[0]]), false);
        assert_eq!(fetch(py, &gh, &clone, &out), (0, vec![]));
    });
    let written: Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    let row = written["prs"].as_object().unwrap().values().next().unwrap();
    assert_eq!(row["branch"], "forge/routing-policy");
    assert_eq!(row["first_push_sha"], shas[0]);
    assert_eq!(row["first_push_ci"], "red");
    assert_eq!(row["commits"][0]["sha"], shas[0]);
    assert_eq!(row["commits"][1]["sha"], shas[1]);
    assert_eq!(row["commits"][0]["trailers"], json!({"Agent":"glm"}));
    assert_eq!(
        row["commits"][1]["trailers"],
        json!({"Agent":"llm-b0","Claude-Session":"https://claude.ai/code/session_01X"})
    );
    assert_eq!(row["commits"][0]["subject"], "feat: first cut");
    assert!(!out.with_file_name(".cache.json.tmp").exists());
}

#[test]
fn an_incremental_run_leaves_cached_prs_alone() {
    let case = isolated_case();
    let (clone, shas) = fixture_repo(&case);
    let out = case.write("cache.json", &rust_fixture().to_string());
    let newer = json!({"number":51,"headRefName":"forge/bytecode","commits":[{"oid":shas[2],"messageHeadline":"feat: newer thing"}]});
    let lists = json!([
        {"number":48,"headRefName":"old","mergedAt":"2026-09-12T09:00:00Z"},
        {"number":51,"headRefName":"forge/bytecode","mergedAt":"2026-09-13T12:00:00Z"}
    ]);
    Python::attach(|py| {
        let gh = client(py, &clone);
        let fake = fake_gh(
            py,
            fake_replies(51, newer, &shas[2], check_runs("green")),
            lists,
            false,
        );
        assert_eq!(fetch(py, &gh, &clone, &out), (0, vec![]));
        assert!(!fake.calls.lock().unwrap().contains(&"view:48".to_owned()));
    });
    let written: Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(written["prs"]["48"], rust_fixture()["prs"]["48"]);
    assert_eq!(written["prs"]["51"]["first_push_ci"], "green");
}

#[test]
fn an_unresolvable_pr_keeps_every_resolvable_one() {
    let case = isolated_case();
    let (clone, shas) = fixture_repo(&case);
    let out = case.root().join("cache.json");
    let newer = json!({"number":51,"headRefName":"forge/bytecode","commits":[{"oid":shas[2],"messageHeadline":"feat: newer thing"}]});
    Python::attach(|py| {
        let gh = client(py, &clone);
        let _fake = fake_gh(
            py,
            fake_replies(51, newer, &shas[2], check_runs("green")),
            recorded_list(),
            true,
        );
        assert_eq!(fetch(py, &gh, &clone, &out), (1, vec![50]));
    });
    let written: Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        written["prs"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["51"]
    );
}

fn patch_ready(py: Python<'_>, message: Option<&str>) -> AttrPatch {
    let class = fetcher(py).getattr("GhClient").unwrap();
    let error = message.map(str::to_owned);
    let callback = PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<()> {
        if let Some(message) = &error {
            let cls = fetcher(args.py()).getattr("EnvError")?;
            return Err(PyErr::from_value(cls.call1((message.as_str(),))?));
        }
        Ok(())
    })
    .unwrap();
    patch_static(class.as_any(), "require_ready", callback.as_any())
}

fn run_main(py: Python<'_>, clone: &Path, out: &Path, dry_run: bool) -> i32 {
    let mut argv = vec![
        "--repo".to_owned(),
        clone.to_str().unwrap().to_owned(),
        "--out".to_owned(),
        out.to_str().unwrap().to_owned(),
    ];
    if dry_run {
        argv.push("--dry-run".to_owned());
    }
    fetcher(py)
        .getattr("main")
        .unwrap()
        .call1((argv,))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn main_exits_2_without_gh_and_writes_nothing() {
    let case = isolated_case();
    let (clone, _) = fixture_repo(&case);
    github_origin(&clone);
    let out = case.root().join("nested/cache.json");
    Python::attach(|py| {
        let _ready = patch_ready(py, Some("gh is not on PATH"));
        let (stderr, _capture) = capture(py, "stderr");
        assert_eq!(run_main(py, &clone, &out, false), 2);
        assert!(captured(&stderr).contains("gh is not on PATH"));
    });
    assert!(!out.exists());
    assert!(!out.parent().unwrap().exists());
}

#[test]
fn main_exits_2_on_an_unauthenticated_gh() {
    let case = isolated_case();
    let (clone, _) = fixture_repo(&case);
    github_origin(&clone);
    let out = case.root().join("cache.json");
    Python::attach(|py| {
        let _ready = patch_ready(
            py,
            Some("gh is not authenticated: To get started with GitHub CLI, run gh auth login"),
        );
        let (stderr, _capture) = capture(py, "stderr");
        assert_eq!(run_main(py, &clone, &out, false), 2);
        assert!(captured(&stderr).contains("gh auth login"));
    });
}

#[test]
fn main_exits_2_on_an_unreadable_existing_cache() {
    let case = isolated_case();
    let (clone, _) = fixture_repo(&case);
    github_origin(&clone);
    let out = case.write("cache.json", "{not json");
    Python::attach(|py| {
        let _ready = patch_ready(py, None);
        let (stderr, _capture) = capture(py, "stderr");
        assert_eq!(run_main(py, &clone, &out, false), 2);
        assert!(captured(&stderr).contains("unreadable"));
    });
    assert_eq!(fs::read_to_string(out).unwrap(), "{not json");
}

#[test]
fn dry_run_calls_no_gh_and_writes_nothing() {
    let case = isolated_case();
    let (clone, _) = fixture_repo(&case);
    github_origin(&clone);
    let fixture = rust_fixture().to_string();
    let out = case.write("cache.json", &fixture);
    Python::attach(|py| {
        let class = fetcher(py).getattr("GhClient").unwrap();
        let forbidden = PyCFunction::new_closure(py, None, None, |_, _| -> PyResult<()> {
            Err(PyAssertionError::new_err("dry-run must not call gh"))
        })
        .unwrap();
        let _run = patch_static(class.as_any(), "_run_gh", forbidden.as_any());
        let _ready = patch_static(class.as_any(), "require_ready", forbidden.as_any());
        let (stdout, _capture) = capture(py, "stdout");
        assert_eq!(run_main(py, &clone, &out, true), 0);
        let plan: Value = serde_json::from_str(&captured(&stdout)).unwrap();
        assert_eq!(plan["dry_run"], true);
        assert_eq!(plan["slug"], "octo/widget");
        assert_eq!(plan["cached_prs_left_alone"], json!(["48", "50"]));
    });
    assert_eq!(fs::read_to_string(out).unwrap(), fixture);
}
