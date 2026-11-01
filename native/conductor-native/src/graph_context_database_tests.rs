use super::*;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-graph-db-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join("graph.db")
    }
    fn db(&self) -> Connection {
        let connection = Connection::open(self.path()).unwrap();
        connection
            .execute_batch("CREATE TABLE nodes(v INTEGER); INSERT INTO nodes VALUES(42);")
            .unwrap();
        connection
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn scope(reader: &Reader) -> QueryScope<'_> {
    reader
        .query_scope(Instant::now() + Duration::from_secs(2), 1_000_000)
        .unwrap()
}

#[test]
fn read_only_wal_and_generation_identity() {
    let f = Fixture::new();
    let writer = f.db();
    writer
        .execute_batch("PRAGMA journal_mode=WAL; INSERT INTO nodes VALUES(7);")
        .unwrap();
    let reader = Reader::open(&f.path()).unwrap().unwrap();
    assert_eq!(
        scope(&reader)
            .query_row("SELECT sum(v) FROM nodes", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        49
    );
    assert!(scope(&reader).execute("DELETE FROM nodes", []).is_err());
    reader.verify().unwrap();
    fs::rename(f.path(), f.0.join("old.db")).unwrap();
    fs::write(f.path(), b"replacement").unwrap();
    assert!(reader.verify().is_err());
}

#[test]
fn unsafe_main_and_ancestor_are_refused() {
    let f = Fixture::new();
    assert!(Reader::open(&f.path()).unwrap().is_none());
    let real = f.0.join("real");
    fs::write(&real, b"sentinel").unwrap();
    symlink(&real, f.path()).unwrap();
    assert!(Reader::open(&f.path()).is_err());
    fs::remove_file(f.path()).unwrap();
    fs::hard_link(&real, f.path()).unwrap();
    assert!(Reader::open(&f.path()).is_err());
    fs::remove_file(f.path()).unwrap();
    let link = f.0.join("link");
    symlink(&f.0, &link).unwrap();
    assert!(Reader::open(&link.join("graph.db")).is_err());
    assert_eq!(fs::read(real).unwrap(), b"sentinel");
}

#[test]
fn fifo_and_unsafe_sidecars_are_refused_without_touching_target() {
    use std::ffi::CString;
    let f = Fixture::new();
    let db = f.db();
    drop(db);
    let target = f.0.join("target");
    fs::write(&target, b"sentinel").unwrap();
    for suffix in ["-journal", "-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", f.path().display()));
        symlink(&target, &side).unwrap();
        assert!(Reader::open(&f.path()).is_err());
        fs::remove_file(&side).unwrap();
        fs::hard_link(&target, &side).unwrap();
        assert!(Reader::open(&f.path()).is_err());
        fs::remove_file(&side).unwrap();
        let name = CString::new(side.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(Reader::open(&f.path()).is_err());
        fs::remove_file(&side).unwrap();
    }
    assert_eq!(fs::read(target).unwrap(), b"sentinel");
    fs::remove_file(f.path()).unwrap();
    let name = CString::new(f.path().as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(Reader::open(&f.path()).is_err());
}

#[test]
fn vm_budget_interrupts_and_guard_clears_hook() {
    let f = Fixture::new();
    drop(f.db());
    let reader = Reader::open(&f.path()).unwrap().unwrap();
    {
        let s = reader
            .query_scope(Instant::now() + Duration::from_secs(2), 1000)
            .unwrap();
        assert!(reader
            .query_scope(Instant::now() + Duration::from_secs(2), 1000)
            .is_err());
        assert!(s.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000000) SELECT sum(x) FROM n", [], |r| r.get::<_, i64>(0)).is_err());
    }
    assert_eq!(
        scope(&reader)
            .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(reader.query_scope(Instant::now(), 1000).is_err());
}

#[test]
fn unlinked_sidecar_is_retried_and_replacement_checked() {
    let f = Fixture::new();
    let path = f.0.join("side");
    let file = File::create(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let mut calls = 0;
    inspect_sidecar(|| {
        calls += 1;
        if calls == 1 {
            file.metadata()
        } else {
            fs::symlink_metadata(&path)
        }
    })
    .unwrap();
    assert_eq!(calls, 2);
    calls = 0;
    assert!(inspect_sidecar(|| {
        calls += 1;
        file.metadata()
    })
    .is_err());
    assert_eq!(calls, 8);
}

#[test]
fn oversized_main_and_sidecar_are_refused() {
    let f = Fixture::new();
    drop(f.db());
    let side = PathBuf::from(format!("{}-wal", f.path().display()));
    File::create(&side)
        .unwrap()
        .set_len(MAX_DB_BYTES + 1)
        .unwrap();
    assert!(Reader::open(&f.path()).is_err());
    fs::remove_file(side).unwrap();
    OpenOptions::new()
        .write(true)
        .open(f.path())
        .unwrap()
        .set_len(MAX_DB_BYTES + 1)
        .unwrap();
    assert!(Reader::open(&f.path()).is_err());
}

#[test]
fn sqlite_exclusive_lock_has_bounded_busy_wait() {
    let f = Fixture::new();
    let writer = f.db();
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let start = Instant::now();
    let result = Reader::open(&f.path());
    if let Ok(Some(reader)) = result {
        assert!(scope(&reader)
            .query_row("SELECT count(*) FROM nodes", [], |r| r.get::<_, i64>(0))
            .is_err());
    }
    assert!(start.elapsed() < Duration::from_secs(3));
    writer.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn nested_operations_preserve_outer_budget_until_owner_drops() {
    let f = Fixture::new();
    drop(f.db());
    let reader = Reader::open(&f.path()).unwrap().unwrap();
    {
        let outer = reader
            .query_scope(Instant::now() + Duration::from_secs(2), 1000)
            .unwrap();
        {
            let inner = reader.operation().unwrap();
            assert_eq!(
                inner
                    .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        assert!(outer.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000000) SELECT sum(x) FROM n", [], |r| r.get::<_, i64>(0)).is_err());
        assert!(reader.operation().is_err());
    }
    assert_eq!(
        reader
            .operation()
            .unwrap()
            .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    {
        let _outer = reader
            .query_scope(Instant::now() + Duration::from_millis(1), 1000)
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(reader.operation().is_err());
    }
    assert!(reader.operation().is_ok());
}

#[test]
fn outer_first_drop_retains_budget_until_last_inner_guard() {
    let f = Fixture::new();
    drop(f.db());
    let reader = Reader::open(&f.path()).unwrap().unwrap();
    let outer = reader
        .query_scope(Instant::now() + Duration::from_secs(2), 1000)
        .unwrap();
    let inner = reader.operation().unwrap();
    drop(outer);
    assert!(reader
        .query_scope(Instant::now() + Duration::from_secs(2), 1000)
        .is_err());
    assert!(inner.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000000) SELECT sum(x) FROM n", [], |r| r.get::<_, i64>(0)).is_err());
    assert!(reader.operation().is_err());
    drop(inner);
    let fresh = reader.operation().unwrap();
    assert_eq!(
        fresh
            .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
