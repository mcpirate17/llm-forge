//! The SQLite test record. A pytest session is written in one transaction, so a
//! reader sees all of a session or none of it.

use rusqlite::{params, Connection, TransactionBehavior};
use std::path::Path;
use std::time::Duration;

pub(crate) const SCHEMA_VERSION: i64 = 1;

// Windows are content-addressed: identical setups across sessions share a row.
// `deps` and `lines` are zstd-compressed JSON.
const SCHEMA: &str = "
CREATE TABLE envs (digest TEXT PRIMARY KEY, data BLOB NOT NULL);
CREATE TABLE windows (
    digest TEXT PRIMARY KEY, kind TEXT NOT NULL, key TEXT NOT NULL, opaque TEXT,
    deps BLOB NOT NULL, lines BLOB NOT NULL);
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY, started REAL NOT NULL, finished REAL NOT NULL,
    root TEXT NOT NULL, env TEXT NOT NULL REFERENCES envs(digest),
    argv TEXT NOT NULL, unstable INTEGER NOT NULL);
CREATE TABLE runs (
    id INTEGER PRIMARY KEY, session INTEGER NOT NULL REFERENCES sessions(id),
    nodeid TEXT NOT NULL, path TEXT NOT NULL, outcome TEXT NOT NULL, opaque TEXT,
    seconds REAL NOT NULL);
CREATE INDEX runs_nodeid ON runs(nodeid, outcome);
CREATE TABLE run_windows (
    run INTEGER NOT NULL REFERENCES runs(id),
    window TEXT NOT NULL REFERENCES windows(digest),
    PRIMARY KEY (run, window)) WITHOUT ROWID;
";

pub(crate) struct WindowRow {
    pub(crate) digest: String,
    pub(crate) kind: &'static str,
    pub(crate) key: String,
    pub(crate) opaque: Option<String>,
    pub(crate) deps: serde_json::Value,
    pub(crate) lines: serde_json::Value,
}

pub(crate) struct RunRow {
    pub(crate) nodeid: String,
    pub(crate) path: String,
    pub(crate) outcome: String,
    pub(crate) opaque: Option<String>,
    pub(crate) seconds: f64,
    pub(crate) windows: Vec<String>,
}

pub(crate) struct SessionRow {
    pub(crate) started: f64,
    pub(crate) finished: f64,
    pub(crate) root: String,
    pub(crate) env_digest: String,
    pub(crate) env_data: serde_json::Value,
    pub(crate) argv: String,
    pub(crate) unstable: usize,
}

fn compressed(value: &serde_json::Value) -> std::io::Result<Vec<u8>> {
    zstd::encode_all(value.to_string().as_bytes(), 3)
}

fn open(path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(60))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    match version {
        0 => {
            connection.execute_batch(&format!(
                "BEGIN IMMEDIATE; {SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
            ))?;
        }
        SCHEMA_VERSION => {}
        other => anyhow::bail!(
            "{}: test record schema {other}, this build reads {SCHEMA_VERSION}",
            path.display()
        ),
    }
    Ok(connection)
}

/// Returns the new session id.
pub(crate) fn write(
    path: &Path,
    session: &SessionRow,
    windows: &[WindowRow],
    runs: &[RunRow],
) -> anyhow::Result<i64> {
    let mut connection = open(path)?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT OR IGNORE INTO envs (digest, data) VALUES (?1, ?2)",
        params![session.env_digest, compressed(&session.env_data)?],
    )?;
    {
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO windows (digest, kind, key, opaque, deps, lines)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for window in windows {
            insert.execute(params![
                window.digest,
                window.kind,
                window.key,
                window.opaque,
                compressed(&window.deps)?,
                compressed(&window.lines)?
            ])?;
        }
    }
    tx.execute(
        "INSERT INTO sessions (started, finished, root, env, argv, unstable)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            session.started,
            session.finished,
            session.root,
            session.env_digest,
            session.argv,
            session.unstable as i64
        ],
    )?;
    let id = tx.last_insert_rowid();
    {
        let mut run = tx.prepare(
            "INSERT INTO runs (session, nodeid, path, outcome, opaque, seconds)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        let mut link =
            tx.prepare("INSERT OR IGNORE INTO run_windows (run, window) VALUES (?1, ?2)")?;
        for row in runs {
            run.execute(params![
                id,
                row.nodeid,
                row.path,
                row.outcome,
                row.opaque,
                row.seconds
            ])?;
            let run_id = tx.last_insert_rowid();
            for window in &row.windows {
                link.execute(params![run_id, window])?;
            }
        }
    }
    tx.commit()?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_is_written_whole_and_windows_are_shared() {
        let dir = std::env::temp_dir().join(format!("test-record-store-{}", std::process::id()));
        let path = dir.join("record.sqlite");
        let _ = std::fs::remove_dir_all(&dir);
        let session = SessionRow {
            started: 1.0,
            finished: 2.0,
            root: "/repo".to_owned(),
            env_digest: "e".to_owned(),
            env_data: serde_json::json!({}),
            argv: "[]".to_owned(),
            unstable: 0,
        };
        let window = || WindowRow {
            digest: "w".to_owned(),
            kind: "session",
            key: String::new(),
            opaque: None,
            deps: serde_json::json!([]),
            lines: serde_json::json!([]),
        };
        let run = RunRow {
            nodeid: "t.py::a".to_owned(),
            path: "t.py".to_owned(),
            outcome: "passed".to_owned(),
            opaque: None,
            seconds: 0.5,
            windows: vec!["w".to_owned()],
        };
        let first = write(&path, &session, &[window()], std::slice::from_ref(&run)).unwrap();
        let second = write(&path, &session, &[window()], &[run]).unwrap();
        assert_ne!(first, second);
        let connection = open(&path).unwrap();
        let count = |table: &str| -> i64 {
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        assert_eq!(
            (count("windows"), count("runs"), count("run_windows")),
            (1, 2, 2)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
