#![cfg(feature = "python-compat-tests")]
//! Rust-owned parity for test_crg_seed_worktree.py (seven cases).

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/worktree_migration_support.rs"]
#[allow(dead_code)]
mod workspace;

use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, text, AttrPatch};
use workspace::{git, WorkspaceCase};

const OLD: &str = "/main/root";
const NEW: &str = "/wt/root";

fn build_store(py: Python<'_>, db: &Path) {
    let conn = module(py, "sqlite3")
        .getattr("connect")
        .unwrap()
        .call1((path(py, db),))
        .unwrap();
    let sql = format!(
        r#"
create table nodes (
 id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, name TEXT NOT NULL,
 qualified_name TEXT NOT NULL UNIQUE, file_path TEXT NOT NULL, line_start INTEGER,
 signature TEXT, extra TEXT DEFAULT '{{}}', updated_at REAL NOT NULL);
create table edges (
 id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL,
 source_qualified TEXT NOT NULL, target_qualified TEXT NOT NULL,
 file_path TEXT NOT NULL, line INTEGER DEFAULT 0);
create table metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
create virtual table nodes_fts using fts5(
 name, qualified_name, file_path, signature, content='nodes', content_rowid='id');
insert into nodes(kind,name,qualified_name,file_path,line_start,signature,extra,updated_at)
 values('Function','seed','{OLD}/conductor/crg_seed_worktree.py::seed',
 '{OLD}/conductor/crg_seed_worktree.py',12,'seed(root={OLD}/x)',
 '{{"root": "{OLD}/conductor"}}',1.0);
insert into nodes(kind,name,qualified_name,file_path,line_start,signature,extra,updated_at)
 values('File','pure','pkg.pure','relative/pure.py',1,'pure()','{{}}',2.0);
insert into edges(kind,source_qualified,target_qualified,file_path,line)
 values('CALLS','{OLD}/a.py::f','{OLD}/b.py::g','{OLD}/a.py',3);
insert into edges(kind,source_qualified,target_qualified,file_path,line)
 values('CALLS','rel::f','rel::g','relative/pure.py',4);
insert into metadata(key,value) values('root','{OLD}/');
insert into nodes_fts(nodes_fts) values('rebuild');
"#
    );
    conn.call_method1("executescript", (sql,)).unwrap();
    conn.call_method0("commit").unwrap();
    conn.call_method0("close").unwrap();
}

fn connection<'py>(py: Python<'py>, db: &Path) -> Bound<'py, PyAny> {
    module(py, "sqlite3")
        .getattr("connect")
        .unwrap()
        .call1((path(py, db),))
        .unwrap()
}

fn cells_with(conn: &Bound<'_, PyAny>, needle: &str) -> BTreeSet<(String, String, String)> {
    let py = conn.py();
    let seeder = module(py, "conductor.crg_seed_worktree");
    let mut found = BTreeSet::new();
    for table in seeder
        .getattr("rewritable_tables")
        .unwrap()
        .call1((conn,))
        .unwrap()
        .try_iter()
        .unwrap()
    {
        let table = text(&table.unwrap());
        let columns: Vec<String> = seeder
            .getattr("text_columns")
            .unwrap()
            .call1((conn, &table))
            .unwrap()
            .extract()
            .unwrap();
        let query = format!("select {} from \"{table}\"", columns.join(", "));
        for row in conn
            .call_method1("execute", (query,))
            .unwrap()
            .try_iter()
            .unwrap()
        {
            let row = row.unwrap();
            for (index, column) in columns.iter().enumerate() {
                let value = row.get_item(index).unwrap();
                if let Ok(value) = value.extract::<String>() {
                    if value.contains(needle) {
                        found.insert((table.clone(), column.clone(), value));
                    }
                }
            }
        }
    }
    found
}

fn store(case: &WorkspaceCase) -> PathBuf {
    let db = case.root().join("graph.db");
    Python::attach(|py| build_store(py, &db));
    db
}

fn scalar_count(conn: &Bound<'_, PyAny>, expression: &str) -> i64 {
    conn.call_method1("execute", (expression,))
        .unwrap()
        .call_method0("fetchone")
        .unwrap()
        .get_item(0)
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn rewrite_replaces_prefix_in_every_text_column_and_spares_other_rows() {
    let case = WorkspaceCase::new();
    let db = store(&case);
    Python::attach(|py| {
        let conn = connection(py, &db);
        let before: BTreeSet<_> = cells_with(&conn, "relative/pure.py")
            .into_iter()
            .filter(|(_, _, value)| !value.contains(OLD))
            .collect();
        let columns: BTreeSet<_> = cells_with(&conn, &format!("{OLD}/"))
            .into_iter()
            .map(|(table, col, _)| (table, col))
            .collect();
        let expected: BTreeSet<_> = [
            ("edges", "file_path"),
            ("edges", "source_qualified"),
            ("edges", "target_qualified"),
            ("metadata", "value"),
            ("nodes", "extra"),
            ("nodes", "file_path"),
            ("nodes", "qualified_name"),
            ("nodes", "signature"),
        ]
        .into_iter()
        .map(|(t, c)| (t.into(), c.into()))
        .collect();
        assert_eq!(columns, expected);
        let seeder = module(py, "conductor.crg_seed_worktree");
        let changed: i64 = seeder
            .getattr("rewrite_root_prefix")
            .unwrap()
            .call1((&conn, format!("{OLD}/"), format!("{NEW}/")))
            .unwrap()
            .extract()
            .unwrap();
        conn.call_method0("commit").unwrap();
        assert!(changed > 0);
        assert!(cells_with(&conn, &format!("{OLD}/")).is_empty());
        let after_columns: BTreeSet<_> = cells_with(&conn, &format!("{NEW}/"))
            .into_iter()
            .map(|(table, col, _)| (table, col))
            .collect();
        assert_eq!(after_columns, columns);
        let after: BTreeSet<_> = cells_with(&conn, "relative/pure.py")
            .into_iter()
            .filter(|(_, _, value)| !value.contains(NEW))
            .collect();
        assert_eq!(after, before);
        conn.call_method0("close").unwrap();
    });
}

#[test]
fn rewrite_skips_fts_virtual_and_shadow_tables() {
    let case = WorkspaceCase::new();
    let db = store(&case);
    Python::attach(|py| {
        let conn = connection(py, &db);
        let names: BTreeSet<String> = conn
            .call_method1(
                "execute",
                ("select name from sqlite_master where type='table'",),
            )
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|row| text(&row.unwrap().get_item(0).unwrap()))
            .collect();
        for name in ["nodes_fts", "nodes_fts_data", "nodes_fts_idx"] {
            assert!(names.contains(name));
        }
        let seeder = module(py, "conductor.crg_seed_worktree");
        let fts: BTreeSet<String> = seeder
            .getattr("fts_table_names")
            .unwrap()
            .call1((&conn,))
            .unwrap()
            .extract()
            .unwrap();
        assert!(fts.contains("nodes_fts") && fts.contains("nodes_fts_data"));
        let tables: BTreeSet<String> = seeder
            .getattr("rewritable_tables")
            .unwrap()
            .call1((&conn,))
            .unwrap()
            .extract::<Vec<String>>()
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(
            tables,
            ["nodes", "edges", "metadata"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );
        conn.call_method0("close").unwrap();
    });
}

#[test]
fn fts_rebuild_reindexes_rewritten_rows() {
    let case = WorkspaceCase::new();
    let db = store(&case);
    Python::attach(|py| {
        let conn = connection(py, &db);
        let seeder = module(py, "conductor.crg_seed_worktree");
        seeder
            .getattr("rewrite_root_prefix")
            .unwrap()
            .call1((&conn, format!("{OLD}/"), format!("{NEW}/")))
            .unwrap();
        conn.call_method0("commit").unwrap();
        let query = "select count(*) from nodes_fts where nodes_fts match '\"wt/root\"'";
        assert_eq!(scalar_count(&conn, query), 0);
        let rebuilt: Vec<String> = seeder
            .getattr("rebuild_fts")
            .unwrap()
            .call1((&conn,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(rebuilt, vec!["nodes_fts"]);
        conn.call_method0("commit").unwrap();
        assert_eq!(scalar_count(&conn, query), 1);
        conn.call_method0("close").unwrap();
    });
}

#[test]
fn copy_refuses_source_wal_unless_forced() {
    let case = WorkspaceCase::new();
    let src = store(&case);
    fs::write(case.root().join("graph.db-wal"), b"").unwrap();
    let dst = case.root().join("wt/.code-review-graph/graph.db");
    Python::attach(|py| {
        let seeder = module(py, "conductor.crg_seed_worktree");
        let copy = seeder.getattr("copy_store").unwrap();
        assert_error(
            py,
            copy.call1((path(py, &src), path(py, &dst))).unwrap_err(),
            &seeder.getattr("SeedError").unwrap(),
            "write is in flight",
        );
        assert!(!dst.exists());
        let kwargs = PyDict::new(py);
        kwargs.set_item("force", true).unwrap();
        copy.call((path(py, &src), path(py, &dst)), Some(&kwargs))
            .unwrap();
        assert_eq!(fs::read(&dst).unwrap(), fs::read(&src).unwrap());
    });
}

#[test]
fn resolve_crg_bin_prefers_override_and_fails_loud_when_absent() {
    let mut case = WorkspaceCase::new();
    case.case.remove_env("CRG_BIN");
    Python::attach(|py| {
        let seeder = module(py, "conductor.crg_seed_worktree");
        let absent = PyCFunction::new_closure(
            py,
            None,
            None,
            |_args, _kwargs| -> PyResult<Option<String>> { Ok(None) },
        )
        .unwrap();
        let _patch = AttrPatch::replace(
            seeder.getattr("shutil").unwrap().as_any(),
            "which",
            absent.as_any(),
        );
        let resolve = seeder.getattr("resolve_crg_bin").unwrap();
        let error = seeder.getattr("SeedError").unwrap();
        assert_error(py, resolve.call0().unwrap_err(), &error, "not on PATH");
        let fake = case.write("code-review-graph", "#!/bin/sh\n");
        case.case.set_env("CRG_BIN", fake.to_str().unwrap());
        assert_eq!(text(&resolve.call0().unwrap()), fake.to_str().unwrap());
        let missing = case.root().join("absent/code-review-graph");
        assert_error(
            py,
            resolve.call1((missing.to_str().unwrap(),)).unwrap_err(),
            &error,
            "not found",
        );
    });
}

fn fixture_worktree(case: &WorkspaceCase) -> PathBuf {
    let wt = case.mkdir("wt");
    fs::write(wt.join("a.py"), "x = 1\n").unwrap();
    git(&wt, &["init", "-q", "-b", "main"]);
    git(&wt, &["config", "user.email", "t@example.com"]);
    git(&wt, &["config", "user.name", "t"]);
    git(&wt, &["config", "commit.gpgsign", "false"]);
    git(&wt, &["config", "core.hooksPath", "/dev/null"]);
    git(&wt, &["add", "a.py"]);
    git(&wt, &["commit", "-qm", "seed"]);
    wt
}

fn fake_crg(py: Python<'_>, crg_bin: &str, stamp: &str) -> AttrPatch {
    let subprocess = module(py, "conductor.crg_seed_worktree")
        .getattr("subprocess")
        .unwrap();
    let real_run = subprocess.getattr("run").unwrap().unbind();
    let crg_bin = crg_bin.to_string();
    let stamp = stamp.to_string();
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            let py = args.py();
            let cmd = args.get_item(0)?;
            let argv: Vec<String> = cmd.extract()?;
            if argv.first().map(String::as_str) != Some(crg_bin.as_str()) {
                return Ok(real_run.bind(py).call(args, kwargs)?.unbind());
            }
            let repo_index = argv
                .iter()
                .position(|value| value == "--repo")
                .expect("CRG update names --repo");
            let worktree = PathBuf::from(&argv[repo_index + 1]);
            let db = worktree.join(".code-review-graph/graph.db");
            module(py, "conductor.crg_seed_worktree")
                .getattr("stamp_head")?
                .call1((path(py, &db), &stamp))?;
            Ok(module(py, "subprocess")
                .getattr("CompletedProcess")?
                .call1((cmd, 0, "", ""))?
                .unbind())
        })
        .unwrap();
    AttrPatch::replace(subprocess.as_any(), "run", callback.as_any())
}

fn seed_case(stamp_head: bool) {
    let case = WorkspaceCase::new();
    let db = case.root().join("main/.code-review-graph/graph.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    Python::attach(|py| build_store(py, &db));
    let wt = fixture_worktree(&case);
    let head = git(&wt, &["rev-parse", "HEAD"]);
    let simulated = if stamp_head {
        "0".repeat(40)
    } else {
        head.clone()
    };
    Python::attach(|py| {
        let seeder = module(py, "conductor.crg_seed_worktree");
        let _patch = fake_crg(py, "/fake/crg", &simulated);
        let report = seeder
            .getattr("seed")
            .unwrap()
            .call1((
                path(py, &case.root().join("main")),
                path(py, &wt),
                "/fake/crg",
            ))
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        let stamped: bool = report
            .get_item("stamped")
            .unwrap()
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(stamped, stamp_head);
        assert_eq!(text(&report.get_item("git_head").unwrap().unwrap()), head);
        if stamp_head {
            assert_eq!(
                text(&report.get_item("graph_head_after_update").unwrap().unwrap()),
                "0".repeat(40)
            );
            assert_eq!(
                text(
                    &seeder
                        .getattr("read_head")
                        .unwrap()
                        .call1((path(py, &wt.join(".code-review-graph/graph.db")),))
                        .unwrap()
                ),
                head
            );
        } else {
            assert_eq!(
                report
                    .get_item("fts_rebuilt")
                    .unwrap()
                    .unwrap()
                    .extract::<Vec<String>>()
                    .unwrap(),
                vec!["nodes_fts"]
            );
        }
    });
}

#[test]
fn seed_stamps_head_when_update_kept_main_checkout_sha() {
    seed_case(true);
}

#[test]
fn seed_reports_no_stamp_when_update_matched_worktree_head() {
    seed_case(false);
}
