use super::*;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

struct Fixture {
    connection: Connection,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "forge-graph-evidence-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join(".forge")).unwrap();
        let connection = Connection::open(root.join(".forge/graph.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE nodes(qualified_name TEXT,name TEXT,file_path TEXT,file_hash TEXT);
            CREATE TABLE edges(source_qualified TEXT,target_qualified TEXT,kind TEXT);
            CREATE TABLE files(file_path TEXT,file_hash TEXT,unresolved INTEGER); BEGIN;",
            )
            .unwrap();
        Self { connection, root }
    }
    fn source(&self, path: &str, bytes: &[u8]) -> String {
        fs::write(self.root.join(path), bytes).unwrap();
        format!("{:x}", Sha256::digest(bytes))
    }
    fn node(&self, name: &str, path: &str, hash: Option<&str>) {
        self.connection
            .execute(
                "INSERT INTO nodes VALUES(?1,?1,?2,?3)",
                params![name, path, hash],
            )
            .unwrap();
        self.connection
            .execute("INSERT INTO files VALUES(?1,?2,0)", params![path, hash])
            .unwrap();
    }
    fn edge(&self, caller: &str, callee: &str) {
        self.connection
            .execute(
                "INSERT INTO edges VALUES(?1,?2,'calls')",
                params![caller, callee],
            )
            .unwrap();
    }
    fn store(&self) -> GraphStore {
        if !self.connection.is_autocommit() {
            self.connection.execute_batch("COMMIT").unwrap();
        }
        GraphStore::open(&self.root, None).unwrap()
    }
    fn target(&self) -> String {
        let hash = self.source("target.py", b"def target(): pass\n");
        self.node("target", "target.py", Some(&hash));
        hash
    }
    fn projection(&self, store: &GraphStore) -> Projection {
        let mut projection = store.project(&self.root, "target.py", None, 100).unwrap();
        store
            .validate_projection(&self.root, "target.py", &mut projection)
            .unwrap();
        projection
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn rows_require_matching_target_and_peer_hashes() {
    let f = Fixture::new();
    f.target();
    let hash = f.source("peer.py", b"def peer(): pass\n");
    f.node("peer", "peer.py", Some(&hash));
    f.edge("peer", "target");
    let store = f.store();
    assert!(store
        .project(&f.root, "target.py", None, 100)
        .unwrap()
        .graph_status
        .starts_with("unverified"));
    let result = f.projection(&store);
    assert_eq!(result.graph_status, "ok");
    assert_eq!(result.callers.len(), 1);
    fs::write(f.root.join("peer.py"), b"changed").unwrap();
    let result = f.projection(&store);
    assert_eq!(result.graph_status, "stale (relationship endpoint differs)");
    assert!(result.callers.is_empty());
}

#[test]
fn missing_invalid_conflicting_hashes_and_case_normalization() {
    let f = Fixture::new();
    let hash = f.target();
    for value in [
        None,
        Some("bad"),
        Some(""),
        Some("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    ] {
        f.connection
            .execute("UPDATE nodes SET file_hash=?1", [value])
            .unwrap();
        let store = f.store();
        assert_eq!(store.indexed_hash(&f.root, "target.py").unwrap(), None);
        let result = f.projection(&store);
        assert!(
            result.graph_status.starts_with("unverified"),
            "{}",
            result.graph_status
        );
        assert!(result.symbols.is_empty());
    }
    f.connection
        .execute("UPDATE nodes SET file_hash=?1", [hash.to_uppercase()])
        .unwrap();
    assert_eq!(
        f.store().indexed_hash(&f.root, "target.py").unwrap(),
        Some(hash)
    );
}

#[test]
fn conflicting_aliases_and_hashes_beyond_symbol_cap_cannot_hide() {
    let f = Fixture::new();
    let hash = f.target();
    let absolute = f.root.join("target.py");
    f.connection
        .execute(
            "INSERT INTO files VALUES(?1,?2,0)",
            params![absolute.to_str(), "b".repeat(64)],
        )
        .unwrap();
    assert_eq!(f.store().indexed_hash(&f.root, "target.py").unwrap(), None);
    f.connection
        .execute("DELETE FROM files WHERE file_path=?1", [absolute.to_str()])
        .unwrap();
    f.connection.execute_batch("BEGIN").unwrap();
    for n in 0..50 {
        f.connection
            .execute(
                "INSERT INTO nodes VALUES(?1,?1,'target.py',?2)",
                params![format!("target{n:02}"), hash],
            )
            .unwrap();
    }
    f.connection
        .execute(
            "INSERT INTO nodes VALUES('zzzz','zzzz','target.py',?1)",
            ["c".repeat(64)],
        )
        .unwrap();
    let store = f.store();
    assert_eq!(
        store
            .project(&f.root, "target.py", None, 100)
            .unwrap()
            .symbols
            .len(),
        41
    );
    assert_eq!(store.indexed_hash(&f.root, "target.py").unwrap(), None);
    assert!(f.projection(&store).symbols.is_empty());
}

#[test]
fn unindexed_and_zero_symbol_files_have_distinct_evidence() {
    let f = Fixture::new();
    let hash = f.source("target.py", b"# empty file\n");
    assert!(f
        .projection(&f.store())
        .graph_status
        .starts_with("unindexed"));
    f.connection
        .execute("INSERT INTO files VALUES('target.py',?1,0)", [hash])
        .unwrap();
    assert_eq!(
        f.projection(&f.store()).graph_status,
        "indexed (no matching symbols)"
    );
    fs::write(f.root.join("target.py"), b"# changed\n").unwrap();
    assert!(f.projection(&f.store()).graph_status.starts_with("stale"));
}

#[test]
fn unsafe_peer_and_fifo_are_excluded_without_blocking() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let f = Fixture::new();
    f.target();
    let path = CString::new(f.root.join("fifo.py").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    f.node("fifo", "fifo.py", Some(&"a".repeat(64)));
    f.node("outside", "../outside.py", Some(&"a".repeat(64)));
    f.edge("fifo", "target");
    f.edge("outside", "target");
    let store = f.store();
    let start = Instant::now();
    let result = f.projection(&store);
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(result.graph_status.starts_with("unverified"));
    assert!(result.callers.is_empty() && result.relationship_limit_reached);
}

#[test]
fn unique_endpoint_cap_bounds_total_hash_work() {
    let f = Fixture::new();
    f.target();
    for n in 0..140 {
        let name = format!("peer{n:03}");
        let path = format!("{name}.py");
        let hash = f.source(&path, b"x");
        f.node(&name, &path, Some(&hash));
        if n < 70 {
            f.edge(&name, "target");
        } else {
            f.edge("target", &name);
        }
    }
    let result = f.projection(&f.store());
    assert_eq!(
        result.graph_status,
        "unverified (relationship source byte budget)"
    );
    assert_eq!(
        result.callers.len() + result.callees.len(),
        MAX_HASHED_FILES - 1
    );
    assert!(result.relationship_limit_reached);
}

#[test]
fn database_change_invalidates_prepared_rows() {
    let f = Fixture::new();
    f.target();
    let store = f.store();
    let mut rows = store.project(&f.root, "target.py", None, 10).unwrap();
    f.connection
        .execute("INSERT INTO edges VALUES('other','target','calls')", [])
        .unwrap();
    assert!(store
        .validate_projection(&f.root, "target.py", &mut rows)
        .is_err());
}

#[test]
fn inherited_vm_budget_interrupts_queries_then_recovers() {
    let f = Fixture::new();
    f.target();
    let store = f.store();
    let scope = store
        .conn
        .as_ref()
        .unwrap()
        .query_scope(Instant::now() + Duration::from_secs(1), 100)
        .unwrap();
    assert!(store.project(&f.root, "target.py", None, 10).is_err());
    drop(scope);
    assert_eq!(f.projection(&store).graph_status, "ok");
}

#[test]
fn views_cannot_substitute_for_graph_tables() {
    let f = Fixture::new();
    f.connection.execute_batch("DROP TABLE nodes; CREATE VIEW nodes AS SELECT 'x' AS qualified_name,'x' AS name,'target.py' AS file_path; COMMIT;").unwrap();
    let error = GraphStore::open(&f.root, None).err().expect("view refused");
    assert!(error.to_string().contains("ordinary table"));
}

#[test]
fn unresolved_alias_conflicts_are_conservative_in_either_row_order() {
    for reverse in [false, true] {
        let fixture = Fixture::new();
        let hash = fixture.target();
        fixture.connection.execute("DELETE FROM files", []).unwrap();
        let mut rows = vec![
            ("target.py".to_owned(), 0),
            (
                fixture.root.join("target.py").to_str().unwrap().to_owned(),
                1,
            ),
        ];
        if reverse {
            rows.reverse();
        }
        for (path, value) in rows {
            fixture
                .connection
                .execute(
                    "INSERT INTO files VALUES(?1,?2,?3)",
                    params![path, hash, value],
                )
                .unwrap();
        }
        let store = fixture.store();
        assert_eq!(
            store.indexed_hash(&fixture.root, "target.py").unwrap(),
            Some(hash)
        );
        assert_eq!(
            store.file_unresolved(&fixture.root, "target.py").unwrap(),
            Some(true)
        );
        assert_eq!(
            store
                .file_unresolved(
                    &fixture.root,
                    fixture.root.join("target.py").to_str().unwrap()
                )
                .unwrap(),
            Some(true)
        );
    }
}

#[test]
fn unresolved_requires_all_present_alias_values_to_be_integer_zero() {
    let fixture = Fixture::new();
    let hash = fixture.target();
    let absolute = fixture.root.join("target.py").to_str().unwrap().to_owned();
    fixture
        .connection
        .execute("INSERT INTO files VALUES(?1,?2,0)", params![absolute, hash])
        .unwrap();
    assert_eq!(
        fixture
            .store()
            .file_unresolved(&fixture.root, "target.py")
            .unwrap(),
        Some(false)
    );
    for invalid in [
        rusqlite::types::Value::Null,
        rusqlite::types::Value::Text("unknown".into()),
        rusqlite::types::Value::Integer(-1),
        rusqlite::types::Value::Real(0.5),
        rusqlite::types::Value::Blob(vec![0]),
    ] {
        fixture
            .connection
            .execute(
                "UPDATE files SET unresolved=?1 WHERE file_path=?2",
                params![invalid, absolute],
            )
            .unwrap();
        assert_eq!(
            fixture
                .store()
                .file_unresolved(&fixture.root, "target.py")
                .unwrap(),
            Some(true)
        );
    }
    fixture.connection.execute("DELETE FROM files", []).unwrap();
    let store = fixture.store();
    assert_eq!(
        store.file_unresolved(&fixture.root, "target.py").unwrap(),
        None
    );
    assert!(store
        .file_unresolved(&fixture.root, "../target.py")
        .is_err());
}
