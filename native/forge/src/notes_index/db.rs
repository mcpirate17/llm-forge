//! Existing SQLite files only. Parent directories and same-UID unconfined
//! processes remain trusted; these checks reject unsafe paths and detect
//! replacement by cooperating writers, rather than providing a custom VFS.
use anyhow::{ensure, Context, Result};
use rusqlite::{config::DbConfig, Connection, OpenFlags};
use std::fs::{self, File, Metadata, OpenOptions};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_DB_BYTES: u64 = 4 * 1024 * 1024 * 1024;

// Drop the SQLite connection before the extra descriptor: closing any descriptor
// for an inode can release this process's POSIX locks on that inode.
pub(super) struct Writer {
    connection: Connection,
    anchor: File,
    path: PathBuf,
}

impl Writer {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let path = checked_path(path).context("notes database not found or unsafe")?;
        let anchor = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)?;
        validate(&anchor.metadata()?, true)?;
        verify_identity(&path, &anchor)?;
        sidecars(&path)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        verify_identity(&path, &anchor)?;
        configure(&connection, true)?;
        reject_index_triggers(&connection)?;
        Ok(Self {
            connection,
            anchor,
            path,
        })
    }

    pub(super) fn rebuild(&mut self, host: &Path, vault: Option<&Path>) -> Result<(u64, u64)> {
        let path = &self.path;
        let anchor = &self.anchor;
        super::rebuild_checked(&mut self.connection, host, vault, || {
            verify_identity(path, anchor)?;
            sidecars(path)
        })
    }
}

/// Resolve dot components while checking every traversed component. In
/// particular, `symlink/../database` must not hide an unsafe ancestor.
fn checked_path(path: &Path) -> Result<PathBuf> {
    ensure!(!path.as_os_str().is_empty(), "notes database path is empty");
    ensure!(
        path.as_os_str().as_bytes().len() <= 4096,
        "notes database path exceeds 4096 bytes"
    );
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut resolved = PathBuf::new();
    let mut components = absolute.components().peekable();
    let mut count = 0;
    while let Some(component) = components.next() {
        count += 1;
        ensure!(count <= 256, "notes database path has too many components");
        match component {
            Component::RootDir => resolved.push("/"),
            Component::CurDir => continue,
            Component::ParentDir => {
                resolved.pop();
                if resolved.as_os_str().is_empty() {
                    resolved.push("/");
                }
            }
            Component::Normal(name) => resolved.push(name),
            Component::Prefix(_) => anyhow::bail!("unsupported notes database path"),
        }
        let meta = fs::symlink_metadata(&resolved)?;
        ensure!(
            !meta.file_type().is_symlink(),
            "notes database path contains a symlink"
        );
        if components.peek().is_some() {
            ensure!(meta.is_dir(), "notes database ancestor is not a directory");
        }
    }
    Ok(resolved)
}

fn validate(meta: &Metadata, owned: bool) -> Result<()> {
    ensure!(
        meta.is_file() && meta.nlink() == 1 && meta.len() <= MAX_DB_BYTES,
        "notes index must be a singly linked regular file at most 4 GiB"
    );
    ensure!(
        !owned || meta.uid() == unsafe { libc::geteuid() },
        "notes database must be owned by this user"
    );
    Ok(())
}

fn verify_identity(path: &Path, anchor: &File) -> Result<()> {
    checked_path(path)?;
    let current = fs::symlink_metadata(path)?;
    let held = anchor.metadata()?;
    validate(&current, true)?;
    validate(&held, true)?;
    ensure!(
        (current.dev(), current.ino()) == (held.dev(), held.ino()),
        "notes database path changed during rebuild"
    );
    Ok(())
}

fn sidecars(path: &Path) -> Result<()> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        match fs::symlink_metadata(Path::new(&name)) {
            Ok(meta) => validate(&meta, true).context("unsafe notes database sidecar")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn configure(connection: &Connection, writable: bool) -> Result<()> {
    connection.busy_timeout(Duration::from_millis(250))?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    connection.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        if writable {
            16 * 1024 * 1024
        } else {
            4 * 1024 * 1024
        },
    );
    connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA cache_size=-2048;")?;
    if writable {
        let page_size: u64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        ensure!(
            (512..=65536).contains(&page_size),
            "invalid notes database page size"
        );
        connection.pragma_update(None, "max_page_count", MAX_DB_BYTES / page_size)?;
    } else {
        connection.execute_batch("PRAGMA query_only=ON;")?;
    }
    let deadline = Instant::now() + Duration::from_secs(if writable { 30 } else { 2 });
    connection.progress_handler(1000, Some(move || Instant::now() >= deadline));
    Ok(())
}

pub(super) fn reject_index_triggers(connection: &Connection) -> Result<()> {
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND
         (lower(tbl_name) IN ('notes_fts','note_tables') OR instr(lower(tbl_name),'notes_fts_')=1)",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        count == 0,
        "notes index tables have custom triggers; rebuild refused"
    );
    Ok(())
}

/// These two tables are owned by the indexer. Do not accept a regular table,
/// external-content FTS index, changed tokenizer, or custom constraints as an
/// equivalent schema merely because a subset of its columns has the right names.
pub(super) fn validate_schema(connection: &Connection) -> Result<()> {
    for (name, expected) in [
        ("notes_fts", super::NOTES_FTS_DDL),
        ("note_tables", super::NOTE_TABLES_DDL),
    ] {
        let (kind, sql): (String, Option<String>) = connection.query_row(
            "SELECT type,sql FROM sqlite_schema WHERE name=?1 COLLATE NOCASE",
            [name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let sql = sql.context("notes index schema has no table definition")?;
        ensure!(
            kind == "table" && normalized_schema(&sql)? == normalized_schema(expected)?,
            "unsupported {name} schema; expected the native or legacy notes index layout"
        );
    }
    Ok(())
}

fn normalized_schema(sql: &str) -> Result<String> {
    ensure!(sql.len() <= 16 * 1024, "notes index schema exceeds 16 KiB");
    let compact: String = sql
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    let compact = compact.trim_end_matches(';');
    for prefix in ["createvirtualtable", "createtable"] {
        if let Some(rest) = compact.strip_prefix(prefix) {
            return Ok(format!(
                "{prefix}{}",
                rest.strip_prefix("ifnotexists").unwrap_or(rest)
            ));
        }
    }
    Ok(compact.to_owned())
}

pub(super) fn open_search(path: &Path) -> Result<Option<Connection>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    validate(&meta, false)?;
    let path = checked_path(path)?;
    sidecars(&path)?;
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let after = fs::symlink_metadata(&path)?;
    validate(&after, false)?;
    ensure!(
        (meta.dev(), meta.ino()) == (after.dev(), after.ino()),
        "notes index changed while opening"
    );
    configure(&connection, false)?;
    Ok(Some(connection))
}
