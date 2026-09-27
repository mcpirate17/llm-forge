//! Rust-owned candidate-review fixtures shared by flow, hardening, and scan contracts.
//!
//! Test bodies and assertions live in Rust. Only production Python modules are
//! imported through PyO3; Python test modules are never fixture providers.

use super::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyString, PyTuple};
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub use super::git_fixture_support::git;

pub const GRANDFATHER_PROBE_PATH: &str = "research/tests/test_legacy_probe.py";
pub const GRANDFATHER_PROBE_LABELS: &[&str] = &["test_frozen_legacy"];

pub fn isolated_case() -> Case {
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

pub fn init_repo(repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "--quiet", "--initial-branch=main"]);
    git(repo, &["config", "user.name", "Candidate Review Test"]);
    git(
        repo,
        &["config", "user.email", "candidate-review@example.invalid"],
    );
    git(repo, &["config", "commit.gpgsign", "false"]);
    git(repo, &["config", "core.hooksPath", "/dev/null"]);
}

pub fn commit_all(repo: &Path, message: &str) -> String {
    git(repo, &["add", "--all"]);
    git(
        repo,
        &[
            "commit",
            "--quiet",
            "--message",
            &format!("{message}\n\nAgent: llm-fixture\n"),
        ],
    );
    git(repo, &["rev-parse", "HEAD"])
}

pub fn added_change<'py>(py: Python<'py>, relative: &str, classes: &[&str]) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("status", "A").unwrap();
    kwargs.set_item("path", relative).unwrap();
    kwargs.set_item("old_path", py.None()).unwrap();
    kwargs.set_item("old_mode", "000000").unwrap();
    kwargs.set_item("new_mode", "100644").unwrap();
    kwargs.set_item("old_oid", "0".repeat(40)).unwrap();
    kwargs.set_item("new_oid", "1".repeat(40)).unwrap();
    kwargs
        .set_item("classes", PyTuple::new(py, classes).unwrap())
        .unwrap();
    module(py, "conductor.candidate_review.model")
        .getattr("Change")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn probe_source(labels: &[&str]) -> String {
    let mut lines = Vec::new();
    for label in labels {
        if let Some((owner, method)) = label.split_once("::") {
            lines.push(format!("class {owner}:"));
            lines.push(format!("    def {method}(self) -> None:"));
            lines.push("        assert True".to_owned());
        } else {
            lines.push(format!("def {label}() -> None:"));
            lines.push("    assert True".to_owned());
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

pub fn crafted_grandfather_inventory(py: Python<'_>, rows: &[(&str, &[&str])]) -> String {
    let verification = module(py, "conductor.candidate_review.verification");
    let policy = module(py, "conductor.candidate_review.policy");
    let version: u32 = verification
        .getattr("GRANDFATHER_SCHEMA_VERSION")
        .unwrap()
        .extract()
        .unwrap();
    let anchor: String = policy
        .getattr("MUTATION_WAIVER_SOURCE_ANCHOR")
        .unwrap()
        .extract()
        .unwrap();
    let milestone: String = policy
        .getattr("W7_TRIDENT_LINEAR_INTEGRATION_MILESTONE")
        .unwrap()
        .extract()
        .unwrap();
    let mut tests = Map::new();
    for (relative, labels) in rows {
        tests.insert((*relative).to_owned(), json!(labels));
    }
    let mut payload = Map::new();
    payload.insert(
        "schema".to_owned(),
        json!(format!(
            "conductor.candidate_review.grandfather_inventory/v{version}"
        )),
    );
    payload.insert("anchor_commit".to_owned(), json!(anchor));
    payload.insert("milestone".to_owned(), json!(milestone));
    payload.insert("tests".to_owned(), Value::Object(tests));
    Value::Object(payload).to_string()
}

pub fn write_grandfather_inventory(py: Python<'_>, snapshot: &Path, text: Option<&str>) -> PathBuf {
    let verification = module(py, "conductor.candidate_review.verification");
    let relative: String = verification
        .getattr("GRANDFATHER_INVENTORY_RELPATH")
        .unwrap()
        .extract()
        .unwrap();
    let target = snapshot.join(&relative);
    let contents = match text {
        Some(text) => text.to_owned(),
        None => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
            fs::read_to_string(root.join(&relative)).unwrap()
        }
    };
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, contents).unwrap();
    target
}

pub fn anchor_snapshot_inventory(
    py: Python<'_>,
    repo: &Path,
    snapshot: &Path,
    rows: &[(&str, &[&str])],
) -> Vec<AttrPatch> {
    init_repo(repo);
    for (relative, labels) in rows {
        let target = repo.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, probe_source(labels)).unwrap();
    }
    let mut add = vec!["add", "--"];
    add.extend(rows.iter().map(|(relative, _)| *relative));
    // An empty inventory still needs an anchor commit.
    if rows.is_empty() {
        git(repo, &["add", "--all"]);
    } else {
        git(repo, &add);
    }
    git(
        repo,
        &["commit", "--quiet", "--allow-empty", "--message", "anchor"],
    );
    let commit = git(repo, &["rev-parse", "HEAD"]);
    let tree = git(repo, &["rev-parse", "HEAD^{tree}"]);
    let verification = module(py, "conductor.candidate_review.verification");
    let mut patches = Vec::new();
    patches.push(AttrPatch::replace(
        &verification,
        "GRANDFATHER_ANCHOR_COMMIT_OID",
        pyo3::types::PyString::new(py, &commit).as_any(),
    ));
    patches.push(AttrPatch::replace(
        &verification,
        "GRANDFATHER_ANCHOR_TREE_OID",
        pyo3::types::PyString::new(py, &tree).as_any(),
    ));
    let inventory = crafted_grandfather_inventory(py, rows);
    let target = write_grandfather_inventory(py, snapshot, Some(&inventory));
    let digest = format!("{:x}", Sha256::digest(fs::read(target).unwrap()));
    patches.push(AttrPatch::replace(
        &verification,
        "GRANDFATHER_INVENTORY_SHA256",
        pyo3::types::PyString::new(py, &digest).as_any(),
    ));
    patches
}

pub fn gate_context<'py>(
    py: Python<'py>,
    root: &Path,
    repo: &Path,
    rows: &[(&str, &[&str])],
    base_oid: &str,
) -> (Bound<'py, PyAny>, Vec<AttrPatch>) {
    let snapshot = root.join("snapshot");
    let registry = snapshot.join("conductor/mutation_campaigns/registry.json");
    fs::create_dir_all(registry.parent().unwrap()).unwrap();
    fs::write(registry, "{}").unwrap();
    let anchor = anchor_snapshot_inventory(py, repo, &snapshot, rows);
    let probe = snapshot.join("research/tests/test_probe.py");
    fs::create_dir_all(probe.parent().unwrap()).unwrap();
    fs::write(probe, "def test_probe_new():\n    assert True\n").unwrap();
    let model = module(py, "conductor.candidate_review.model");
    let candidate_kwargs = PyDict::new(py);
    candidate_kwargs.set_item("kind", "index").unwrap();
    candidate_kwargs
        .set_item("tree_oid", "a".repeat(40))
        .unwrap();
    candidate_kwargs
        .set_item("base_tree_oid", "b".repeat(40))
        .unwrap();
    candidate_kwargs
        .set_item("base_commit_oid", base_oid)
        .unwrap();
    candidate_kwargs.set_item("commit_oid", py.None()).unwrap();
    candidate_kwargs.set_item("target_ref", "HEAD").unwrap();
    candidate_kwargs
        .set_item(
            "changes",
            PyTuple::new(
                py,
                [added_change(
                    py,
                    "research/tests/test_probe.py",
                    &["python", "source", "test"],
                )],
            )
            .unwrap(),
        )
        .unwrap();
    let candidate = model
        .getattr("Candidate")
        .unwrap()
        .call((), Some(&candidate_kwargs))
        .unwrap();
    let policy_path = module(py, "conductor.candidate_review.policy_path")
        .getattr("resolve_policy_path")
        .unwrap()
        .call0()
        .unwrap();
    let policy = module(py, "conductor.candidate_review.policy")
        .getattr("load_policy")
        .unwrap()
        .call1((policy_path,))
        .unwrap();
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo", path(py, repo)).unwrap();
    kwargs.set_item("snapshot", path(py, &snapshot)).unwrap();
    kwargs.set_item("candidate", candidate).unwrap();
    kwargs.set_item("entries", PyTuple::empty(py)).unwrap();
    kwargs.set_item("policy", policy).unwrap();
    kwargs.set_item("surface", "manual").unwrap();
    kwargs.set_item("profile", "fast").unwrap();
    kwargs.set_item("owner", py.None()).unwrap();
    kwargs
        .set_item("runtime_dir", path(py, &root.join("runtime")))
        .unwrap();
    let context = module(py, "conductor.candidate_review.checks")
        .getattr("ReviewContext")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap();
    (context, anchor)
}

pub fn fixture_receipt<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let finding = json!({
        "check_id":"candidate-integrity","rule_id":"unsafe-symlink",
        "severity":"critical","message":"candidate symlink escapes its snapshot",
        "path":"escape","line":2,"column":0,
        "help":"use a repository-relative target","evidence":{},
        "fingerprint":"test-fingerprint-not-a-secret","exception_id":null
    });
    let payload = json!({
        "schema_version":1,"receipt_id":"","receipt_digest":"",
        "surface":"manual","profile":"fast","decision":"fail",
        "candidate":{"kind":"commit","tree_oid":"a".repeat(40),"base_tree_oid":"b".repeat(40),
            "base_commit_oid":"c".repeat(40),"commit_oid":"d".repeat(40),
            "target_ref":"HEAD","changes":[]},
        "policy":{"sha256":"e".repeat(64)},"engine":{"candidate_source_sha256":"f".repeat(64)},
        "graph":{},"bypass":{},"timings":{"duration_ms":125},
        "cache":{"hits":0,"misses":1},"baselines":[],
        "checks":[{"check_id":"candidate-integrity","status":"failed","duration_ms":125,
            "findings":[finding.clone()],"stdout_tail":"","stderr_tail":"bad candidate"}],
        "findings":[finding],"binding":"0".repeat(64)
    });
    let values = module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((payload.to_string(),))
        .unwrap();
    let model = module(py, "conductor.candidate_review.model");
    let kwargs = values.cast::<PyDict>().unwrap();
    let receipt = model
        .getattr("ReviewReceipt")
        .unwrap()
        .call((), Some(kwargs))
        .unwrap();
    model
        .getattr("seal_receipt")
        .unwrap()
        .call1((receipt,))
        .unwrap()
}

pub fn write_fixture(repo: &Path, relative: &str, contents: &str) -> PathBuf {
    let target = repo.join(relative);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, contents).unwrap();
    target
}

pub fn utc_date(py: Python<'_>, day_offset: i32) -> String {
    let datetime = module(py, "datetime");
    let utc = datetime.getattr("UTC").unwrap();
    let now = datetime
        .getattr("datetime")
        .unwrap()
        .call_method1("now", (utc,))
        .unwrap();
    let days = datetime
        .getattr("timedelta")
        .unwrap()
        .call(
            (),
            Some(&{
                let kwargs = PyDict::new(py);
                kwargs.set_item("days", day_offset).unwrap();
                kwargs
            }),
        )
        .unwrap();
    now.call_method1("__add__", (days,))
        .unwrap()
        .call_method0("date")
        .unwrap()
        .call_method0("isoformat")
        .unwrap()
        .extract()
        .unwrap()
}

pub fn minimal_policy_text(expires: &str, exceptions: &str) -> String {
    format!(
        r#"schema_version = 1
block_at = "high"
max_workers = 1
cache_ttl_days = 1
claim_max_age_hours = 1
max_file_bytes = 1000000
max_binary_bytes = 1000000
coverage_threshold = 75.0
high_risk_coverage_threshold = 90.0
baseline_expires = {expires}
{exceptions}

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
"#
    )
}

pub fn write_policy(py: Python<'_>, repo: &Path, relative: &str) -> PathBuf {
    write_fixture(
        repo,
        relative,
        &minimal_policy_text(&utc_date(py, 30), "exceptions = []"),
    )
}

pub fn install_candidate_engine(repo: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/conductor/candidate_review");
    let target = repo.join("conductor/candidate_review");
    fs::create_dir_all(&target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().is_some_and(|ext| ext == "py") {
            fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}

pub fn load_policy<'py>(py: Python<'py>, file: &Path) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.policy")
        .getattr("load_policy")
        .unwrap()
        .call1((path(py, file),))
        .unwrap()
}

pub fn default_policy<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let file = module(py, "conductor.candidate_review.policy_path")
        .getattr("resolve_policy_path")
        .unwrap()
        .call0()
        .unwrap();
    module(py, "conductor.candidate_review.policy")
        .getattr("load_policy")
        .unwrap()
        .call1((file,))
        .unwrap()
}

pub fn resolve_candidate<'py>(
    py: Python<'py>,
    repo: &Path,
    kind: &str,
    base_ref: Option<&str>,
    target_ref: Option<&str>,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("kind", kind).unwrap();
    if let Some(base) = base_ref {
        kwargs.set_item("base_ref", base).unwrap();
    }
    if let Some(target) = target_ref {
        kwargs.set_item("target_ref", target).unwrap();
    }
    module(py, "conductor.candidate_review.git_source")
        .getattr("resolve_candidate")
        .unwrap()
        .call((path(py, repo),), Some(&kwargs))
        .unwrap()
}

pub fn classify_candidate<'py>(
    py: Python<'py>,
    candidate: &Bound<'py, PyAny>,
    policy: &Bound<'py, PyAny>,
) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.git_source")
        .getattr("classify_candidate")
        .unwrap()
        .call1((candidate, policy))
        .unwrap()
}

pub fn replace_fields<'py>(
    py: Python<'py>,
    object: &Bound<'py, PyAny>,
    kwargs: &Bound<'py, PyDict>,
) -> Bound<'py, PyAny> {
    module(py, "dataclasses")
        .getattr("replace")
        .unwrap()
        .call((object,), Some(kwargs))
        .unwrap()
}

pub fn with_materialized<'py, R>(
    py: Python<'py>,
    repo: &Path,
    tree_oid: &str,
    max_blob_bytes: Option<u64>,
    body: impl FnOnce(&Path, &Bound<'py, PyAny>) -> R,
) -> R {
    let kwargs = PyDict::new(py);
    if let Some(limit) = max_blob_bytes {
        kwargs.set_item("max_blob_bytes", limit).unwrap();
    }
    let manager = module(py, "conductor.candidate_review.git_source")
        .getattr("materialize_tree")
        .unwrap()
        .call((path(py, repo), tree_oid), Some(&kwargs))
        .unwrap();
    let entered = manager.call_method0("__enter__").unwrap();
    let snapshot: PathBuf = entered.get_item(0).unwrap().extract().unwrap();
    let entries = entered.get_item(1).unwrap();
    let guard = ContextExit(manager.unbind());
    let result = body(&snapshot, &entries);
    drop(guard);
    result
}

struct ContextExit(Py<PyAny>);

impl Drop for ContextExit {
    fn drop(&mut self) {
        Python::attach(|py| {
            self.0
                .bind(py)
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .expect("close materialized snapshot");
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub fn review_context<'py>(
    py: Python<'py>,
    repo: &Path,
    snapshot: &Path,
    candidate: &Bound<'py, PyAny>,
    entries: &Bound<'py, PyAny>,
    policy: &Bound<'py, PyAny>,
    surface: &str,
    profile: &str,
    owner: Option<&str>,
    runtime_dir: &Path,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo", path(py, repo)).unwrap();
    kwargs.set_item("snapshot", path(py, snapshot)).unwrap();
    kwargs.set_item("candidate", candidate).unwrap();
    kwargs.set_item("entries", entries).unwrap();
    kwargs.set_item("policy", policy).unwrap();
    kwargs.set_item("surface", surface).unwrap();
    kwargs.set_item("profile", profile).unwrap();
    kwargs.set_item("owner", owner).unwrap();
    kwargs
        .set_item("runtime_dir", path(py, runtime_dir))
        .unwrap();
    module(py, "conductor.candidate_review.checks")
        .getattr("ReviewContext")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn string_attr(object: &Bound<'_, PyAny>, name: &str) -> String {
    object.getattr(name).unwrap().extract().unwrap()
}

pub fn attr_strings(object: &Bound<'_, PyAny>, name: &str) -> Vec<String> {
    object.getattr(name).unwrap().extract().unwrap()
}

pub fn py_string<'py>(py: Python<'py>, value: &str) -> Bound<'py, PyAny> {
    PyString::new(py, value).into_any()
}

pub fn write_adversarial_sources(repo: &Path) {
    write_fixture(repo, "research/data/protected.txt", "protected\n");
    commit_all(repo, "protected baseline");
    git(repo, &["rm", "research/data/protected.txt"]);
    let mechanism = concat!(
        "# TODO remove scaffold\nimport json, pickle, subprocess, yaml\nimport random\n",
        "def pending(): pass\ndef missing(): ...\ndef unsafe(items):\n",
        "    for outer in items:\n        for inner in items:\n            json.loads('{}')\n",
        "    eval('1')\n    pickle.loads(b'x')\n    yaml.load('x')\n",
        "    subprocess.run('x', shell=True)\n    raise NotImplementedError()\n",
        "RANDOM = random.random()\n",
        "def softmax_fallback(): return 'attention fallback'\n",
        "# performance-critical pure Python\n"
    );
    for (relative, contents) in [
        ("component_fab/harness/new_mechanism.py", mechanism),
        ("research/tools/result_record_probe.py", "stage1_passed = True\nscore = 1\ndtype = 'bad'\n"),
        ("duplicate_a.py", "def copied(value):\n    value += 1\n    value += 2\n    value += 3\n    value += 4\n    value += 5\n    value += 6\n    value += 7\n    value += 8\n    return value\n"),
        ("duplicate_b.py", "def copied_again(value):\n    value += 1\n    value += 2\n    value += 3\n    value += 4\n    value += 5\n    value += 6\n    value += 7\n    value += 8\n    return value\n"),
        ("secrets.py", &format!("{} = {:?}\n", "api_".to_owned() + "key", "A".repeat(32))),
        ("package.json", "{}\n"),
        ("settings.json", "{\n"),
        ("research/runtime/native/bad.c", "void bad(char *x) { system(x); strcpy(x, x); }\n"),
    ] {
        write_fixture(repo, relative, contents);
    }
    write_fixture(
        repo,
        "probe.ipynb",
        r#"{"nbformat":4,"cells":[{"cell_type":"code","outputs":[{"text":"x"}],"execution_count":1}]}"#,
    );
    fs::write(repo.join("artifact.pt"), b"binary").unwrap();
    git(repo, &["add", "--all"]);
}

pub fn test_selection<'py>(py: Python<'py>, tests: &[&str]) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("tests", PyTuple::new(py, tests).unwrap())
        .unwrap();
    kwargs.set_item("graph", PyDict::new(py)).unwrap();
    kwargs.set_item("findings", PyTuple::empty(py)).unwrap();
    module(py, "conductor.candidate_review.checks")
        .getattr("TestSelection")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn coverage_fixture(repo: &Path) {
    write_fixture(
        repo,
        "probe.py",
        "def a():\n    return 1\n\n\ndef b():\n    return 1\n",
    );
    commit_all(repo, "baseline");
    write_fixture(
        repo,
        "probe.py",
        "def a():\n    return 2\n\n\ndef b():\n    return 3\n",
    );
    write_fixture(repo, "conductor/__init__.py", "");
    write_fixture(
        repo,
        "conductor/test_a.py",
        "from probe import a\n\n\ndef test_a_boundary_property():\n    assert a() == 2\n",
    );
    write_fixture(
        repo,
        "conductor/test_b.py",
        "from probe import b\n\n\ndef test_b_boundary_property():\n    assert b() == 3\n",
    );
    git(
        repo,
        &[
            "add",
            "probe.py",
            "conductor/__init__.py",
            "conductor/test_a.py",
            "conductor/test_b.py",
        ],
    );
}

pub fn write_graph_database(repo: &Path, base: &str) {
    let db = repo.join(".code-review-graph/graph.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = Connection::open(db).unwrap();
    conn.execute_batch(
        "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT);\
        CREATE TABLE nodes (qualified_name TEXT PRIMARY KEY, file_path TEXT, is_test INTEGER);\
        CREATE TABLE edges (source_qualified TEXT, target_qualified TEXT, kind TEXT);",
    )
    .unwrap();
    for (key, value) in [
        ("git_head_sha", base),
        ("schema_version", "test"),
        ("last_updated", "now"),
    ] {
        conn.execute("INSERT INTO metadata VALUES (?, ?)", params![key, value])
            .unwrap();
    }
    for (name, file, is_test) in [
        ("probe:value", "probe.py", 0),
        ("test:value", "conductor/test_graph_probe.py", 1),
        ("rust:probe", "native/lib.rs", 1),
    ] {
        let path = repo.join(file).canonicalize().unwrap();
        conn.execute(
            "INSERT INTO nodes VALUES (?, ?, ?)",
            params![name, path.to_str().unwrap(), is_test],
        )
        .unwrap();
    }
    for (source, target) in [("test:value", "probe:value"), ("rust:probe", "probe:value")] {
        conn.execute(
            "INSERT INTO edges VALUES (?, ?, ?)",
            params![source, target, "calls"],
        )
        .unwrap();
    }
}
