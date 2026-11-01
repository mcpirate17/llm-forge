//! Bounded read-only SQLite admission. Operator-owned parent directories and
//! same-UID mutation remain trusted; this is not a descriptor-rooted SQLite VFS.
use anyhow::{ensure, Context, Result};
use rusqlite::{config::DbConfig, limits::Limit, Connection, OpenFlags};
use std::cell::Cell;
use std::fs::{self, File, Metadata, OpenOptions};
use std::ops::Deref;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const MAX_DB_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const INTERVAL: u64 = 100;

// Connection must be dropped before anchor: POSIX locks may be released by
// closing any descriptor for the inode, including one opened independently.
pub struct Reader {
    connection: Connection,
    anchor: File,
    path: PathBuf,
    deadline: Cell<Option<Instant>>,
    guards: Cell<usize>,
    exhausted: Arc<AtomicBool>,
}

pub struct QueryScope<'a> {
    connection: &'a Connection,
    deadline: &'a Cell<Option<Instant>>,
    guards: &'a Cell<usize>,
    exhausted: &'a AtomicBool,
}
impl Deref for QueryScope<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.connection
    }
}
impl Drop for QueryScope<'_> {
    fn drop(&mut self) {
        let remaining = self.guards.get() - 1;
        self.guards.set(remaining);
        if remaining == 0 {
            self.connection.progress_handler(0, None::<fn() -> bool>);
            self.deadline.set(None);
            self.exhausted.store(false, Ordering::Relaxed);
        }
    }
}

impl Deref for Reader {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.connection
    }
}

impl Reader {
    pub fn open(path: &Path) -> Result<Option<Self>> {
        let Some(path) = checked_path(path)? else {
            return Ok(None);
        };
        let anchor = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
            .context("open graph database anchor")?;
        validate(&anchor.metadata()?)?;
        verify(&path, &anchor)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .context("open graph database read-only")?;
        let reader = Self {
            connection,
            anchor,
            path,
            deadline: Cell::new(None),
            guards: Cell::new(0),
            exhausted: Arc::new(AtomicBool::new(false)),
        };
        reader.verify()?;
        {
            let scope = reader.query_scope(Instant::now() + Duration::from_secs(2), 2_000_000)?;
            scope.busy_timeout(Duration::from_millis(250))?;
            scope.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
            scope.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
            scope.set_limit(Limit::SQLITE_LIMIT_LENGTH, 4 * 1024 * 1024);
            scope.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 64 * 1024);
            scope.set_limit(Limit::SQLITE_LIMIT_COLUMN, 256);
            scope.set_limit(Limit::SQLITE_LIMIT_EXPR_DEPTH, 128);
            scope.execute_batch(
                "PRAGMA query_only=ON; PRAGMA cache_size=-2048; PRAGMA temp_store=FILE;",
            )?;
        }
        reader.verify()?;
        Ok(Some(reader))
    }
    pub fn connection(&self) -> &Connection {
        &self.connection
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn verify(&self) -> Result<()> {
        verify(&self.path, &self.anchor)
    }
    pub fn operation(&self) -> Result<QueryScope<'_>> {
        if let Some(deadline) = self.deadline.get() {
            ensure!(
                Instant::now() < deadline && !self.exhausted.load(Ordering::Relaxed),
                "graph query operation budget exhausted"
            );
            let count = self
                .guards
                .get()
                .checked_add(1)
                .context("graph query guard count overflow")?;
            self.guards.set(count);
            return Ok(QueryScope {
                connection: &self.connection,
                deadline: &self.deadline,
                guards: &self.guards,
                exhausted: &self.exhausted,
            });
        }
        self.query_scope(Instant::now() + Duration::from_secs(2), 20_000_000)
    }
    pub fn query_scope(&self, deadline: Instant, max_vm_ops: u64) -> Result<QueryScope<'_>> {
        ensure!(
            self.guards.get() == 0,
            "nested explicit graph query scopes are not allowed"
        );
        ensure!(
            deadline > Instant::now() && max_vm_ops >= INTERVAL,
            "invalid graph query deadline or operation budget"
        );
        let mut remaining = max_vm_ops / INTERVAL;
        self.exhausted.store(false, Ordering::Relaxed);
        let exhausted = Arc::clone(&self.exhausted);
        self.connection.progress_handler(
            INTERVAL as i32,
            Some(move || {
                remaining = remaining.saturating_sub(1);
                let stop = remaining == 0 || Instant::now() >= deadline;
                if stop {
                    exhausted.store(true, Ordering::Relaxed);
                }
                stop
            }),
        );
        self.deadline.set(Some(deadline));
        self.guards.set(1);
        Ok(QueryScope {
            connection: &self.connection,
            deadline: &self.deadline,
            guards: &self.guards,
            exhausted: &self.exhausted,
        })
    }
}

fn checked_path(path: &Path) -> Result<Option<PathBuf>> {
    ensure!(
        !path.as_os_str().is_empty() && path.as_os_str().as_bytes().len() <= 4096,
        "invalid graph database path length"
    );
    ensure!(
        !path.as_os_str().as_bytes().contains(&0)
            && path.components().all(|part| matches!(
                part,
                Component::RootDir | Component::CurDir | Component::Normal(_)
            )),
        "graph database path contains NUL or traversal"
    );
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut current = PathBuf::new();
    for (index, component) in absolute.components().enumerate() {
        ensure!(index < 256, "graph database path component limit exceeded");
        match component {
            Component::RootDir => current.push("/"),
            Component::CurDir => continue,
            Component::Normal(name) => current.push(name),
            _ => anyhow::bail!("graph database path contains traversal"),
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink(),
                "graph database path contains symlink"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Some(current))
}
fn validate(meta: &Metadata) -> Result<()> {
    ensure!(
        meta.is_file() && meta.nlink() == 1 && meta.len() <= MAX_DB_BYTES,
        "graph database or sidecar must be a singly linked regular file at most 4 GiB"
    );
    Ok(())
}
fn verify(path: &Path, anchor: &File) -> Result<()> {
    ensure!(checked_path(path)?.is_some(), "graph database disappeared");
    let held = anchor.metadata()?;
    let current = fs::symlink_metadata(path)?;
    validate(&held)?;
    validate(&current)?;
    ensure!(
        (held.dev(), held.ino()) == (current.dev(), current.ino()),
        "graph database identity changed"
    );
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        inspect_sidecar(|| fs::symlink_metadata(Path::new(&name)))?;
    }
    Ok(())
}
fn inspect_sidecar(mut read: impl FnMut() -> std::io::Result<Metadata>) -> Result<()> {
    for _ in 0..8 {
        match read() {
            Ok(meta) if meta.is_file() && meta.nlink() == 0 && meta.len() <= MAX_DB_BYTES => {
                continue
            }
            Ok(meta) => return validate(&meta),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("graph database sidecar is busy changing; retry")
}

#[cfg(test)]
#[path = "graph_context_database_tests.rs"]
mod tests;
