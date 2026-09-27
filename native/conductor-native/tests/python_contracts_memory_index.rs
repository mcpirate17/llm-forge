#![cfg(feature = "python-compat-tests")]
//! Rust-authored cases for the public memory-index Python contract.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::sync::{Arc, Mutex};
use support::{assert_error, attr_text, module, path, text, AttrPatch, Case};

fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    PyModule::import(py, "json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let encoded: String = PyModule::import(value.py(), "json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

fn kwargs<'py>(py: Python<'py>, pairs: &[(&str, Bound<'py, PyAny>)]) -> Bound<'py, PyDict> {
    let out = PyDict::new(py);
    for (key, value) in pairs {
        out.set_item(key, value).unwrap();
    }
    out
}

fn fingerprint() -> String {
    format!("sha256:{}", "a".repeat(64))
}

fn meta(gpu: i64) -> Value {
    json!({"fingerprint":fingerprint(),"dimension":2,"paid":false,"num_gpu":gpu,"num_ctx":2048})
}

fn row(file: &str, source: &str, digest: &str, body: &str) -> Value {
    json!({"schema_version":3,"embedding":meta(0),"source_sha256":digest,
        "source":source,"path":file,"title":"t","text":body,"vector":[1.0,0.0]})
}

fn memory_error(
    py: Python<'_>,
    mi: &Bound<'_, PyModule>,
    result: PyResult<Bound<'_, PyAny>>,
    message: &str,
) {
    assert_error(
        py,
        result.expect_err("expected memory index error"),
        &mi.getattr("RetrieveError").unwrap(),
        message,
    );
}

fn fixed_embed<'py>(
    py: Python<'py>,
    vector: Vec<f64>,
    calls: Option<Arc<Mutex<Vec<String>>>>,
) -> Bound<'py, PyCFunction> {
    PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<Vec<f64>> {
            if let Some(calls) = &calls {
                calls
                    .lock()
                    .unwrap()
                    .push(args.get_item(0)?.extract::<String>()?);
            }
            Ok(vector.clone())
        },
    )
    .unwrap()
}

fn python_set<'py>(py: Python<'py>, values: &[&str]) -> Bound<'py, PyAny> {
    PyModule::import(py, "builtins")
        .unwrap()
        .getattr("set")
        .unwrap()
        .call1((values.to_vec(),))
        .unwrap()
}

fn capture<'py>(py: Python<'py>, stream: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let buffer = PyModule::import(py, "io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(
        PyModule::import(py, "sys").unwrap().as_any(),
        stream,
        &buffer,
    );
    (buffer, patch)
}

#[test]
fn source_iteration_exclusions_and_include_dirs() {
    let case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        case.write("notes/kb_law.md", "law");
        case.write("notes/finding.md", "evidence");
        let entry = py_json(
            py,
            &json!({"id":"notes","kind":"index","absolute_root":case.root().join("notes").display().to_string(),"glob":"*.md","exclude_globs":["kb_*.md"]}),
        );
        let files = mi
            .getattr("iter_source_files")
            .unwrap()
            .call1((entry,))
            .unwrap();
        let names = files
            .call_method0("__iter__")
            .unwrap()
            .call_method0("__next__")
            .unwrap();
        assert_eq!(attr_text(&names, "name"), "finding.md");
        assert!(files
            .call_method0("__iter__")
            .unwrap()
            .call_method0("__next__")
            .is_err());

        case.write("projects/real-project/memory/a-fact.md", "fact");
        case.write("projects/real-project/memory/MEMORY.md", "pointer list");
        case.write("projects/real-project/memory/archive/an-old-fact.md", "old");
        case.write("projects/scratchpad-session/memory/scratch.md", "scratch");
        let entry = py_json(
            py,
            &json!({"id":"claude-memory","kind":"index","absolute_root":case.root().join("projects").display().to_string(),
            "include_dirs":["real-project/memory"],"glob":"*.md","exclude_globs":["MEMORY.md"]}),
        );
        let list = PyModule::import(py, "builtins")
            .unwrap()
            .getattr("list")
            .unwrap()
            .call1((mi
                .getattr("iter_source_files")
                .unwrap()
                .call1((&entry,))
                .unwrap(),))
            .unwrap();
        let mut names = list
            .try_iter()
            .unwrap()
            .map(|p| attr_text(&p.unwrap(), "name"))
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["a-fact.md", "an-old-fact.md"]);
        for excluded in [
            "projects/real-project/memory/MEMORY.md",
            "projects/scratchpad-session/memory/scratch.md",
        ] {
            assert!(!mi
                .getattr("path_matches_source")
                .unwrap()
                .call1((&entry, path(py, &case.root().join(excluded))))
                .unwrap()
                .extract::<bool>()
                .unwrap());
        }
    });
}

#[test]
fn source_path_matching_dir_glob_and_default_markdown() {
    let case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let root = case.root().display().to_string();
        for (relative, extra, want) in [
            (
                "node_modules/a.md",
                json!({"exclude_dir_names":["node_modules"],"glob":"*.md"}),
                false,
            ),
            (
                "kb_secret.md",
                json!({"exclude_globs":["kb_*.md"],"glob":"*.md"}),
                false,
            ),
            ("a.md", json!({}), true),
        ] {
            let file = case.write(relative, "x");
            let mut entry = json!({"kind":"index","absolute_root":root});
            entry
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let actual: bool = mi
                .getattr("path_matches_source")
                .unwrap()
                .call1((py_json(py, &entry), path(py, &file)))
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(actual, want, "{relative}");
        }
    });
}

#[test]
fn injected_query_ranking_and_both_invalid_top_k_values() {
    let _case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let rows = py_json(
            py,
            &json!([
            {"num_gpu":0,"source":"notes","path":"a.md","title":"other","text":"throughput","vector":[0.0,1.0]},
            {"num_gpu":0,"source":"notes","path":"b.md","title":"eager","text":"EAGER_REQUIRED stays","vector":[1.0,0.0]}]),
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let embed = fixed_embed(py, vec![1.0, 0.0], Some(Arc::clone(&calls)));
        for top_k in [0i32, -1] {
            let kw = kwargs(
                py,
                &[
                    ("top_k", top_k.into_pyobject(py).unwrap().into_any()),
                    ("embedder", embed.clone().into_any()),
                ],
            );
            memory_error(
                py,
                &mi,
                mi.getattr("query_index")
                    .unwrap()
                    .call(("q", &rows), Some(&kw)),
                "top_k must be",
            );
        }
        let kw = kwargs(
            py,
            &[
                ("top_k", 1i32.into_pyobject(py).unwrap().into_any()),
                ("embedder", embed.into_any()),
            ],
        );
        let hits = mi
            .getattr("query_index")
            .unwrap()
            .call(("what is eager", rows), Some(&kw))
            .unwrap();
        assert_eq!(json_value(&hits)[0]["path"], "b.md");
        assert!(calls.lock().unwrap()[0].starts_with(&text(
            &module(py, "conductor.kb_retrieve")
                .getattr("QUERY_INSTRUCT")
                .unwrap()
        )));
        let rows = py_json(
            py,
            &json!([
                row("low.md", "notes", &"a".repeat(64), "low"),
                row("high.md", "notes", &"a".repeat(64), "high")
            ]),
        );
        rows.get_item(0)
            .unwrap()
            .set_item("vector", vec![0.0, 1.0])
            .unwrap();
        let kw = kwargs(
            py,
            &[
                ("top_k", 1i32.into_pyobject(py).unwrap().into_any()),
                ("embedder", fixed_embed(py, vec![1.0, 0.0], None).into_any()),
            ],
        );
        let hits = mi
            .getattr("query_index")
            .unwrap()
            .call(("q", rows), Some(&kw))
            .unwrap();
        assert_eq!(hits.len().unwrap(), 1);
        assert_eq!(json_value(&hits)[0]["path"], "high.md");
    });
}

#[test]
fn load_and_decode_rows_validate_schema_dimension_and_guest_gpu() {
    let case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let mut guest = row("a.md", "notes", &"a".repeat(64), "x");
        guest["embedding"] = meta(99);
        let file = case.write("memory_index.jsonl", &format!("{}\n", guest));
        let loaded = mi
            .getattr("load_index")
            .unwrap()
            .call1((path(py, &file),))
            .unwrap();
        assert_eq!(json_value(&loaded)[0]["embedding"]["num_gpu"], 99);
        let mut invalid = guest.clone();
        invalid["schema_version"] = json!(999);
        memory_error(
            py,
            &mi,
            mi.getattr("_decode_index_row")
                .unwrap()
                .call1((invalid.to_string(),)),
            "unsupported memory index schema",
        );
        invalid["schema_version"] = json!(3);
        invalid["vector"] = json!([1.0, 2.0, 3.0]);
        memory_error(
            py,
            &mi,
            mi.getattr("_decode_index_row")
                .unwrap()
                .call1((invalid.to_string(),)),
            "dimension disagrees",
        );
    });
}

#[test]
fn row_grouping_reuse_and_partition_accounting() {
    let case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let rows = py_json(
            py,
            &json!([
            {"source":"notes","path":"a.md","n":1},{"source":"notes","path":"a.md","n":2},
            {"source":"notes","path":"b.md","n":3}]),
        );
        let grouped = mi.getattr("_rows_by_path").unwrap().call1((rows,)).unwrap();
        assert_eq!(grouped.len().unwrap(), 2);
        assert_eq!(
            json_value(&grouped.get_item(("notes", "a.md")).unwrap()),
            json!([
            {"source":"notes","path":"a.md","n":1},{"source":"notes","path":"a.md","n":2}])
        );
        let source = case.write("note.md", "hello");
        let prior = PyDict::new(py);
        prior
            .set_item(
                ("notes", source.display().to_string()),
                py_json(py, &json!([{"source":"notes","path":source.display().to_string(),"text":"hello","vector":[1.0],"source_sha256":"expected"}])),
            )
            .unwrap();
        let reuse = mi.getattr("_reuse_rows").unwrap();
        assert!(!reuse
            .call1(("notes", path(py, &source), &prior, "expected"))
            .unwrap()
            .is_none());
        assert!(reuse
            .call1(("notes", path(py, &source), &prior, "changed"))
            .unwrap()
            .is_none());
        let partition = case.write(
            "partition.jsonl",
            &[
                row("a.md", "notes", &"a".repeat(64), "x"),
                row("b.md", "cards", &"a".repeat(64), "x"),
                row("c.md", "stale", &"a".repeat(64), "x"),
            ]
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        );
        let kw = kwargs(
            py,
            &[
                ("selected_sources", python_set(py, &["notes"])),
                ("available_sources", python_set(py, &["notes", "cards"])),
            ],
        );
        let result = mi
            .getattr("load_index_partition")
            .unwrap()
            .call((path(py, &partition),), Some(&kw))
            .unwrap();
        assert_eq!(
            json_value(&result.get_item(0).unwrap())[0]["source"],
            "notes"
        );
        assert_eq!(result.get_item(1).unwrap().extract::<usize>().unwrap(), 1);
        assert_eq!(result.get_item(2).unwrap().extract::<usize>().unwrap(), 1);
    });
}

#[test]
fn catalog_schema_sources_and_live_current_work_exclusion() {
    let case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let catalog = case.write(
            "sources.toml",
            "schema_version = 999\n[[source]]\nid = \"x\"\n",
        );
        memory_error(
            py,
            &mi,
            mi.getattr("load_catalog")
                .unwrap()
                .call1((path(py, &catalog),)),
            "unsupported catalog schema",
        );
        fs::write(&catalog, "schema_version = 1\nsource = []\nsources = []\n").unwrap();
        memory_error(
            py,
            &mi,
            mi.getattr("load_catalog")
                .unwrap()
                .call1((path(py, &catalog),)),
            "no [[source]] entries",
        );
        let bundled = mi.getattr("load_catalog").unwrap().call0().unwrap();
        let entries = json_value(&bundled)["source"].as_array().unwrap().clone();
        assert!(!entries
            .iter()
            .any(|v| v["kind"] == "index" && v["id"] == "current-work"));
        let claude = entries.iter().find(|v| v["id"] == "claude-memory").unwrap();
        assert_eq!(claude["kind"], "index");
        assert_eq!(claude["absolute_root"], "/home/tim/.claude/projects");
        assert!(claude["exclude_globs"]
            .as_array()
            .unwrap()
            .contains(&json!("MEMORY.md")));
        assert!(claude["include_dirs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.as_str().unwrap().ends_with("/memory")));
        let vault = entries.iter().find(|v| v["id"] == "vault-unique").unwrap();
        assert!(vault["exclude_dir_names"]
            .as_array()
            .unwrap()
            .contains(&json!("memory")));
    });
}

#[test]
fn save_index_atomic_replace_and_total_rows() {
    let case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let file = case.write("memory.jsonl", "old\n");
        let rows = py_json(py, &json!([{"schema_version":3,"value":"new"}]));
        assert_eq!(
            text(
                &mi.getattr("save_index")
                    .unwrap()
                    .call1((rows, path(py, &file)))
                    .unwrap()
            ),
            file.display().to_string()
        );
        assert_eq!(
            serde_json::from_str::<Value>(&fs::read_to_string(&file).unwrap()).unwrap(),
            json!({"schema_version":3,"value":"new"})
        );
        assert_eq!(
            fs::read_dir(case.root())
                .unwrap()
                .filter(|p| p
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp"))
                .count(),
            0
        );
        let class = mi.getattr("IndexBuildResult").unwrap();
        let kw = kwargs(
            py,
            &[
                ("rows", py_json(py, &json!([{"source":"notes"}]))),
                (
                    "changed",
                    true.into_pyobject(py).unwrap().to_owned().into_any(),
                ),
                (
                    "selected_sources",
                    ("notes",).into_pyobject(py).unwrap().into_any(),
                ),
                ("reused_count", 0usize.into_pyobject(py).unwrap().into_any()),
                (
                    "embedded_count",
                    1usize.into_pyobject(py).unwrap().into_any(),
                ),
                (
                    "preserved_count",
                    3usize.into_pyobject(py).unwrap().into_any(),
                ),
                (
                    "removed_count",
                    0usize.into_pyobject(py).unwrap().into_any(),
                ),
            ],
        );
        let result = class.call((), Some(&kw)).unwrap();
        assert_eq!(
            result
                .getattr("total_rows")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            4
        );
    });
}

fn two_source_catalog(case: &Case) -> std::path::PathBuf {
    case.write("sources.toml",&format!(
        "schema_version = 1\n[[source]]\nid = \"notes\"\nkind = \"index\"\nabsolute_root = {}\nglob = \"*.md\"\nchunk = \"heading\"\n[[source]]\nid = \"cards\"\nkind = \"index\"\nabsolute_root = {}\nglob = \"*.md\"\nchunk = \"whole\"\n",
        json!(case.root().join("notes").display().to_string()),
        json!(case.root().join("cards").display().to_string())
    ))
}

#[test]
fn partial_refresh_preserves_other_sources_and_reuses_unchanged_note() {
    let case = Case::new();
    let note = case.write("notes/finding.md", "new finding");
    let card = case.write("cards/kb_rule.md", "stable rule");
    let catalog = two_source_catalog(&case);
    let index = case.root().join("index.jsonl");
    let card_digest = format!("{:x}", Sha256::digest(fs::read(&card).unwrap()));
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let rows = py_json(
            py,
            &json!([
                row(
                    &note.display().to_string(),
                    "notes",
                    &"0".repeat(64),
                    "new finding"
                ),
                row(
                    &card.display().to_string(),
                    "cards",
                    &card_digest,
                    "stable rule"
                )
            ]),
        );
        mi.getattr("save_index")
            .unwrap()
            .call1((rows, path(py, &index)))
            .unwrap();
        let kw = kwargs(
            py,
            &[
                ("source_ids", python_set(py, &["notes"])),
                ("catalog_path", path(py, &catalog)),
                ("index_path", path(py, &index)),
                ("embedder", fixed_embed(py, vec![0.0, 1.0], None).into_any()),
            ],
        );
        let result = mi
            .getattr("build_index_result")
            .unwrap()
            .call((), Some(&kw))
            .unwrap();
        assert!(result
            .getattr("changed")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(
            result
                .getattr("preserved_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1
        );
        assert_eq!(
            result
                .getattr("embedded_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1
        );
        let materialized = json_value(&result.call_method0("materialize_rows").unwrap());
        let mut sources = materialized
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["source"].as_str().unwrap())
            .collect::<Vec<_>>();
        sources.sort();
        assert_eq!(sources, vec!["cards", "notes"]);
        mi.getattr("save_index_result")
            .unwrap()
            .call1((&result, path(py, &index)))
            .unwrap();
        let calls = Arc::new(Mutex::new(0usize));
        let seen = Arc::clone(&calls);
        let no_embed = PyCFunction::new_closure(
            py,
            None,
            None,
            move |_args: &Bound<'_, PyTuple>,
                  _kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Vec<f64>> {
                *seen.lock().unwrap() += 1;
                Ok(vec![1.0, 0.0])
            },
        )
        .unwrap();
        let kw = kwargs(
            py,
            &[
                ("source_ids", python_set(py, &["notes"])),
                ("catalog_path", path(py, &catalog)),
                ("index_path", path(py, &index)),
                ("embedder", no_embed.into_any()),
            ],
        );
        let no_op = mi
            .getattr("build_index_result")
            .unwrap()
            .call((), Some(&kw))
            .unwrap();
        assert!(!no_op.getattr("changed").unwrap().extract::<bool>().unwrap());
        assert_eq!(
            no_op
                .getattr("preserved_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1
        );
        assert_eq!(
            no_op
                .getattr("reused_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1
        );
        assert_eq!(*calls.lock().unwrap(), 0);
    });
}

#[test]
fn partial_refresh_requires_an_existing_full_index() {
    let case = Case::new();
    case.write("notes/finding.md", "finding");
    case.write("cards/kb_rule.md", "rule");
    let catalog = two_source_catalog(&case);
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let kw = kwargs(
            py,
            &[
                ("source_ids", python_set(py, &["notes"])),
                ("catalog_path", path(py, &catalog)),
                ("index_path", path(py, &case.root().join("missing.jsonl"))),
                ("embedder", fixed_embed(py, vec![1.0], None).into_any()),
            ],
        );
        memory_error(
            py,
            &mi,
            mi.getattr("build_index_result")
                .unwrap()
                .call((), Some(&kw)),
            "existing full index",
        );
    });
}

#[test]
fn index_write_lock_excludes_other_holder_and_releases_after_exception() {
    let case = Case::new();
    let file = case.root().join("cache/memory_index.jsonl");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let fcntl = PyModule::import(py, "fcntl").unwrap();
        let lock_path = file.parent().unwrap().join(".memory_index.jsonl.lock");
        let holder = PyModule::import(py, "builtins")
            .unwrap()
            .getattr("open")
            .unwrap()
            .call1((path(py, &lock_path), "a+"))
            .unwrap();
        let fd = holder.call_method0("fileno").unwrap();
        let exclusive: i32 = fcntl.getattr("LOCK_EX").unwrap().extract().unwrap();
        let nonblocking: i32 = fcntl.getattr("LOCK_NB").unwrap().extract().unwrap();
        fcntl
            .getattr("flock")
            .unwrap()
            .call1((&fd, exclusive | nonblocking))
            .unwrap();
        let kw = kwargs(
            py,
            &[("timeout", 0.2f64.into_pyobject(py).unwrap().into_any())],
        );
        let ctx = mi
            .getattr("index_write_lock")
            .unwrap()
            .call((path(py, &file),), Some(&kw))
            .unwrap();
        memory_error(py, &mi, ctx.call_method0("__enter__"), "timed out");
        let unlock: i32 = fcntl.getattr("LOCK_UN").unwrap().extract().unwrap();
        fcntl
            .getattr("flock")
            .unwrap()
            .call1((&fd, unlock))
            .unwrap();
        holder.call_method0("close").unwrap();
        for exceptional in [false, true, false] {
            let kw = kwargs(
                py,
                &[("timeout", 1.0f64.into_pyobject(py).unwrap().into_any())],
            );
            let ctx = mi
                .getattr("index_write_lock")
                .unwrap()
                .call((path(py, &file),), Some(&kw))
                .unwrap();
            ctx.call_method0("__enter__").unwrap();
            let exception = if exceptional {
                pyo3::exceptions::PyRuntimeError::new_err("boom")
                    .into_value(py)
                    .into_bound(py)
                    .into_any()
            } else {
                py.None().into_bound(py)
            };
            let kind = if exceptional {
                exception.get_type().into_any()
            } else {
                py.None().into_bound(py)
            };
            let exited = ctx.call_method1("__exit__", (kind, exception, py.None()));
            if exceptional {
                assert!(
                    !exited.unwrap().extract::<bool>().unwrap(),
                    "body error must not be suppressed"
                );
            } else {
                assert!(!exited.unwrap().extract::<bool>().unwrap());
            }
        }
    });
}

fn build_result<'py>(
    py: Python<'py>,
    mi: &Bound<'py, PyModule>,
    rows: Value,
    changed: bool,
    sources: Vec<&str>,
    counts: [usize; 3],
) -> Bound<'py, PyAny> {
    let [reused, embedded, preserved] = counts;
    let kw = kwargs(
        py,
        &[
            ("rows", py_json(py, &rows)),
            (
                "changed",
                changed.into_pyobject(py).unwrap().to_owned().into_any(),
            ),
            (
                "selected_sources",
                PyTuple::new(py, sources).unwrap().into_any(),
            ),
            ("reused_count", reused.into_pyobject(py).unwrap().into_any()),
            (
                "embedded_count",
                embedded.into_pyobject(py).unwrap().into_any(),
            ),
            (
                "preserved_count",
                preserved.into_pyobject(py).unwrap().into_any(),
            ),
            (
                "removed_count",
                0usize.into_pyobject(py).unwrap().into_any(),
            ),
        ],
    );
    mi.getattr("IndexBuildResult")
        .unwrap()
        .call((), Some(&kw))
        .unwrap()
}

#[test]
fn main_index_routes_sources_full_save_and_reports_counts() {
    let case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let target = case.root().join("cache/memory_index.jsonl");
        let _index = AttrPatch::replace(mi.as_any(), "INDEX_PATH", &path(py, &target));
        let nullcontext = PyModule::import(py, "contextlib")
            .unwrap()
            .getattr("nullcontext")
            .unwrap();
        let _lock = AttrPatch::replace(mi.as_any(), "index_write_lock", &nullcontext);
        let result = build_result(
            py,
            &mi,
            json!([{"a":1},{"a":2}]),
            true,
            vec!["notes"],
            [1, 1, 0],
        );
        let build_calls = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&build_calls);
        let built = result.clone().unbind();
        let build = PyCFunction::new_closure(py, None, None, move |args: &Bound<'_, PyTuple>, kw: Option<&Bound<'_, PyDict>>| -> PyResult<Py<PyAny>> {
            let kw = kw.unwrap();
            let mut selected: Vec<String> = kw.get_item("source_ids")?.unwrap().try_iter()?
                .map(|item| item?.extract::<String>()).collect::<PyResult<_>>()?;
            selected.sort();
            observed.lock().unwrap().push(json!({"source_ids":selected,"incremental":kw.get_item("incremental")?.unwrap().extract::<bool>()?}));
            Ok(built.clone_ref(args.py()))
        })
        .unwrap();
        let saved = Arc::new(Mutex::new(0usize));
        let count = Arc::clone(&saved);
        let expected = result.clone().unbind();
        let dest = target.clone();
        let save = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  _kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                assert!(args.get_item(0)?.is(expected.bind(args.py())));
                *count.lock().unwrap() += 1;
                fs::create_dir_all(dest.parent().unwrap()).unwrap();
                fs::write(&dest, "saved\n").unwrap();
                Ok(path(args.py(), &dest).unbind())
            },
        )
        .unwrap();
        let _patches = (
            AttrPatch::replace(mi.as_any(), "build_index_result", build.as_any()),
            AttrPatch::replace(mi.as_any(), "save_index_result", save.as_any()),
        );
        let (stdout, _stream) = capture(py, "stdout");
        assert_eq!(
            mi.getattr("main")
                .unwrap()
                .call1((vec!["index", "--sources", "notes, other", "--full"],))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert_eq!(
            build_calls.lock().unwrap().as_slice(),
            [json!({"source_ids":["notes","other"],"incremental":false})]
        );
        assert_eq!(*saved.lock().unwrap(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&text(
                &stdout.getattr("getvalue").unwrap().call0().unwrap()
            ))
            .unwrap(),
            json!({"index":target.display().to_string(),"chunks":2,"sources":["notes"],
                   "updated":true,"reused":1,"embedded":1,"preserved":0,"removed":0})
        );
    });
}

#[test]
fn main_index_skips_save_when_unchanged_and_cache_exists() {
    let case = Case::new();
    let target = case.write("cache/memory_index.jsonl", "existing\n");
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let _index = AttrPatch::replace(mi.as_any(), "INDEX_PATH", &path(py, &target));
        let nullcontext = PyModule::import(py, "contextlib")
            .unwrap()
            .getattr("nullcontext")
            .unwrap();
        let _lock = AttrPatch::replace(mi.as_any(), "index_write_lock", &nullcontext);
        let built = build_result(py, &mi, json!([]), false, vec![], [0, 0, 0]).unbind();
        let build = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  _kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> { Ok(built.clone_ref(args.py())) },
        )
        .unwrap();
        let save = PyCFunction::new_closure(
            py,
            None,
            None,
            move |_args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
                Err(pyo3::exceptions::PyAssertionError::new_err(
                    "save_index_result called on no-op",
                ))
            },
        )
        .unwrap();
        let _patches = (
            AttrPatch::replace(mi.as_any(), "build_index_result", build.as_any()),
            AttrPatch::replace(mi.as_any(), "save_index_result", save.as_any()),
        );
        let (_stdout, _stream) = capture(py, "stdout");
        assert_eq!(
            mi.getattr("main")
                .unwrap()
                .call1((vec!["index"],))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
    });
}

#[test]
fn main_query_uses_given_virtual_index_and_reports_hits() {
    let case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen = Arc::clone(&calls);
        let load = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  _kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                seen.lock()
                    .unwrap()
                    .push(json!({"load_path":text(&args.get_item(0)?)}));
                Ok(py_json(args.py(), &json!([{"path":"a.md"}])).unbind())
            },
        )
        .unwrap();
        let seen = Arc::clone(&calls);
        let query = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                seen.lock().unwrap().push(
                    json!({"query":text(&args.get_item(0)?),"rows":json_value(&args.get_item(1)?),
                "top_k":kw.unwrap().get_item("top_k")?.unwrap().extract::<usize>()?}),
                );
                Ok(py_json(args.py(), &json!([{"name":"card","text":"hit"}])).unbind())
            },
        )
        .unwrap();
        let _patches = (
            AttrPatch::replace(mi.as_any(), "load_index", load.as_any()),
            AttrPatch::replace(mi.as_any(), "query_index", query.as_any()),
        );
        let (stdout, _stream) = capture(py, "stdout");
        let file = case.root().join("custom_index.jsonl");
        let args = vec![
            "query".to_owned(),
            "how do I authenticate".to_owned(),
            "--top-k".to_owned(),
            "3".to_owned(),
            "--index".to_owned(),
            file.display().to_string(),
        ];
        assert_eq!(
            mi.getattr("main")
                .unwrap()
                .call1((args,))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [
                json!({"load_path":file.display().to_string()}),
                json!({"query":"how do I authenticate","rows":[{"path":"a.md"}],"top_k":3})
            ]
        );
        assert_eq!(
            serde_json::from_str::<Value>(&text(
                &stdout.getattr("getvalue").unwrap().call0().unwrap()
            ))
            .unwrap(),
            json!([{"name":"card","text":"hit"}])
        );
    });
}

#[test]
fn main_reports_retrieve_error_from_both_query_paths() {
    let case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let err_class = mi.getattr("RetrieveError").unwrap().unbind();
        let fail = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
                Err(PyErr::from_value(
                    err_class
                        .bind(args.py())
                        .call1(("index produced zero chunks",))?,
                ))
            },
        )
        .unwrap();
        let _patches = (
            AttrPatch::replace(mi.as_any(), "load_index", fail.as_any()),
            AttrPatch::replace(mi.as_any(), "query_index_file", fail.as_any()),
        );
        let target = case.root().join("query.jsonl");
        for exists in [false, true] {
            if exists {
                fs::write(&target, "{}\n").unwrap();
            }
            let (stderr, stream) = capture(py, "stderr");
            let args = vec![
                "query".to_owned(),
                "anything".to_owned(),
                "--index".to_owned(),
                target.display().to_string(),
            ];
            assert_eq!(
                mi.getattr("main")
                    .unwrap()
                    .call1((args,))
                    .unwrap()
                    .extract::<i32>()
                    .unwrap(),
                2
            );
            assert_eq!(
                serde_json::from_str::<Value>(&text(
                    &stderr.getattr("getvalue").unwrap().call0().unwrap()
                ))
                .unwrap(),
                json!({"error":"index produced zero chunks"})
            );
            drop(stream);
        }
    });
}

#[test]
fn expand_root_resolves_configured_notes_and_literal_relative_roots() {
    let case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let _cwd = case.chdir(".");
        case.write(
            "pyproject.toml",
            "[tool.conductor]\nnotes_root = \"cards\"\n",
        );
        let notes = mi
            .getattr("_expand_root")
            .unwrap()
            .call1((py_json(py, &json!({"id":"notes","root":"research/notes"})),))
            .unwrap();
        assert_eq!(
            text(&notes),
            case.root().join("cards").display().to_string()
        );
        let tasks = mi
            .getattr("_expand_root")
            .unwrap()
            .call1((py_json(py, &json!({"id":"tasks","root":"tasks"})),))
            .unwrap();
        assert_eq!(
            text(&tasks),
            case.root().join("tasks").display().to_string()
        );
        case.write(
            "pyproject.toml",
            "[tool.conductor]\nnotes_root = \"research/notes\"\n",
        );
        let notes = mi
            .getattr("_expand_root")
            .unwrap()
            .call1((py_json(py, &json!({"id":"notes","root":"research/notes"})),))
            .unwrap();
        assert_eq!(
            text(&notes),
            case.root().join("research/notes").display().to_string()
        );
    });
}

#[test]
fn host_catalog_precedence_and_auto_index_reader_agree() {
    let mut case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let auto = module(py, "conductor.memory_auto_index");
        let packaged = text(&mi.getattr("PACKAGED_SOURCES_PATH").unwrap());
        assert_eq!(
            text(
                &mi.getattr("host_catalog_path")
                    .unwrap()
                    .call1((path(py, case.root()),))
                    .unwrap()
            ),
            packaged
        );
        let host = case.write(
            "conductor/memory_sources.toml",
            "schema_version = 1\n[[source]]\nid = \"x\"\nkind = \"index\"\nroot = \".\"\n",
        );
        assert_eq!(
            text(
                &mi.getattr("host_catalog_path")
                    .unwrap()
                    .call1((path(py, case.root()),))
                    .unwrap()
            ),
            host.display().to_string()
        );
        case.set_env("CONDUCTOR_MEMORY_SOURCES", "cfg/absent.toml");
        memory_error(
            py,
            &mi,
            mi.getattr("host_catalog_path")
                .unwrap()
                .call1((path(py, case.root()),)),
            "does not exist",
        );
        case.remove_env("CONDUCTOR_MEMORY_SOURCES");
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = Arc::clone(&seen);
        let load = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  _kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                recorded.lock().unwrap().push(text(&args.get_item(0)?));
                Ok(py_json(args.py(), &json!({"source":[]})).unbind())
            },
        )
        .unwrap();
        let _patch = AttrPatch::replace(auto.as_any(), "load_catalog", load.as_any());
        let kw = kwargs(py, &[("repo_root", path(py, case.root()))]);
        auto.getattr("indexed_path_sources")
            .unwrap()
            .call((py_json(py, &json!({})),), Some(&kw))
            .unwrap();
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [host.display().to_string()]
        );
    });
}
