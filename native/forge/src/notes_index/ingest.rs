//! Capability-scoped, bounded reads and atomic replacement of both notes indexes.
use super::{
    ddl,
    extract::{extract_tables, title_of, to_python_json},
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, File, MetadataExt, OpenOptions, OpenOptionsExt};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub files: usize,
    pub examined: usize,
    pub note_bytes: usize,
    pub total_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            files: 10_000,
            examined: 100_000,
            note_bytes: 2 * 1024 * 1024,
            total_bytes: 128 * 1024 * 1024,
        }
    }
}

struct Source {
    tag: &'static str,
    root: PathBuf,
    relative_to: PathBuf,
    enabled: bool,
}
struct Counts {
    files: usize,
    tables: u64,
    examined: usize,
    bytes: usize,
}
struct Note {
    text: String,
    mtime: f64,
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    ensure!(
        joined.as_os_str().len() <= 4096 && joined.components().count() <= 256,
        "notes source root exceeds path limits"
    );
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                ensure!(
                    open_root(&result)?.is_some(),
                    "notes parent path traverses a missing directory"
                );
                ensure!(result.pop(), "notes root escapes filesystem root");
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

fn options(directory: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(
        libc::O_NOFOLLOW | libc::O_NONBLOCK | if directory { libc::O_DIRECTORY } else { 0 },
    );
    options
}

// Resolve one component at a time, so no ancestor symlink can redirect reads.
fn open_root(path: &Path) -> Result<Option<Dir>> {
    let mut directory = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    for part in path.components() {
        let Component::Normal(name) = part else {
            continue;
        };
        let file = match directory.open_with(name, &options(true)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("open notes root {}", path.display()))
            }
        };
        directory = Dir::from_std_file(file.into_std());
    }
    Ok(Some(directory))
}

fn sources(host: &Path, vault: Option<&Path>) -> Result<Vec<Source>> {
    let host = absolute(host)?;
    let mut sources = vec![
        Source {
            tag: "notes",
            root: absolute(
                &conductor_native::project_paths::notes_root(&host).map_err(anyhow::Error::msg)?,
            )?,
            relative_to: host.clone(),
            enabled: true,
        },
        Source {
            tag: "tasks",
            root: host.join("tasks"),
            relative_to: host.clone(),
            enabled: true,
        },
    ];
    if let Some(vault) = vault {
        let vault = absolute(vault)?;
        let enabled = open_root(&vault.join("research"))?.is_some();
        for (tag, child) in [
            ("vault_research", "research"),
            ("vault_dashboards", "dashboards"),
            ("vault_runbooks", "runbooks"),
        ] {
            let relative_to = if vault.starts_with(&host) {
                host.clone()
            } else {
                vault.clone()
            };
            sources.push(Source {
                tag,
                root: vault.join(child),
                relative_to,
                enabled,
            });
        }
    }
    Ok(sources)
}

fn previous_source(conn: &Connection, source: &str) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM notes_fts WHERE source=?1) OR EXISTS(SELECT 1 FROM note_tables WHERE source=?1)", [source], |row| row.get(0))?)
}

fn read_note(mut file: File, limits: Limits, counts: &mut Counts) -> Result<Note> {
    let before = file.metadata()?;
    ensure!(before.is_file(), "note must be a regular file");
    ensure!(
        before.len() <= limits.note_bytes as u64,
        "note exceeds per-file byte limit"
    );
    let mtime = before
        .modified()?
        .into_std()
        .duration_since(UNIX_EPOCH)
        .context("note modification time predates Unix epoch")?
        .as_secs_f64();
    let mut text = String::new();
    (&mut file)
        .take(limits.note_bytes as u64 + 1)
        .read_to_string(&mut text)
        .context("note must be readable UTF-8")?;
    ensure!(
        text.len() <= limits.note_bytes && !text.contains('\0'),
        "note exceeds byte limit or contains NUL"
    );
    let after = file.metadata()?;
    ensure!(
        before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "note changed while reading"
    );
    counts.bytes = counts
        .bytes
        .checked_add(text.len())
        .context("notes byte count overflow")?;
    ensure!(
        counts.bytes <= limits.total_bytes,
        "notes exceed aggregate byte limit"
    );
    Ok(Note { text, mtime })
}

fn insert_note(
    tx: &Transaction<'_>,
    source: &Source,
    path: &Path,
    note: Note,
    now: f64,
) -> Result<u64> {
    let relative = path
        .strip_prefix(&source.relative_to)
        .context("note is outside source base")?;
    let relative = relative.to_str().context("note path must be UTF-8")?;
    let basename = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("note filename must be UTF-8")?;
    tx.execute(
        "INSERT INTO notes_fts (path,source,title,body,mtime) VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![
            relative,
            source.tag,
            title_of(&note.text, path),
            note.text,
            note.mtime
        ],
    )?;
    let tables = extract_tables(&note.text);
    for table in &tables {
        tx.execute("INSERT INTO note_tables (source,path,note,table_idx,section_heading,n_cols,n_rows,headers_json,rows_json,ingested_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![source.tag,relative,basename,table.table_idx,table.section,table.headers.len() as i64,
                table.rows.len() as i64,to_python_json(&table.headers)?,to_python_json(&table.rows)?,now])?;
    }
    Ok(tables.len() as u64)
}

fn scan(
    tx: &Transaction<'_>,
    source: &Source,
    root: Dir,
    limits: Limits,
    counts: &mut Counts,
    now: f64,
) -> Result<()> {
    let mut files = enumerate(source, &root, limits, counts)?;
    files.sort();
    for path in files {
        let directory = descendant(&root, path.parent().context("note parent is missing")?)?;
        let file = directory.open_with(
            path.file_name().context("note filename is missing")?,
            &options(false),
        )?;
        let absolute = source.root.join(&path);
        let note = read_note(file, limits, counts)
            .with_context(|| format!("read note {}", absolute.display()))?;
        counts.tables += insert_note(tx, source, &absolute, note, now)?;
    }
    Ok(())
}

fn enumerate(
    source: &Source,
    root: &Dir,
    limits: Limits,
    counts: &mut Counts,
) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let directory = descendant(root, &relative)?;
        for entry in directory.entries()? {
            counts.examined = counts
                .examined
                .checked_add(1)
                .context("notes entry count overflow")?;
            ensure!(
                counts.examined <= limits.examined,
                "notes exceed examined-entry limit"
            );
            let entry = entry?;
            let path = relative.join(entry.file_name());
            ensure!(
                path.as_os_str().len() <= 4096,
                "note path exceeds 4096 bytes"
            );
            let absolute = source.root.join(&path);
            let is_audit = absolute
                .strip_prefix(&source.relative_to)
                .is_ok_and(|rel| rel.starts_with("tasks/audit"));
            if is_audit {
                continue;
            }
            let kind = entry.file_type()?;
            ensure!(
                !kind.is_symlink(),
                "symlinked notes descendants are forbidden: {}",
                absolute.display()
            );
            if kind.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                ensure!(
                    kind.is_file(),
                    "note must be a regular file: {}",
                    absolute.display()
                );
                counts.files = counts
                    .files
                    .checked_add(1)
                    .context("notes file count overflow")?;
                ensure!(
                    counts.files <= limits.files,
                    "notes exceed file-count limit"
                );
                files.push(path);
            }
        }
    }
    Ok(files)
}

fn descendant(root: &Dir, relative: &Path) -> Result<Dir> {
    let mut directory = root.try_clone()?;
    for part in relative.components() {
        let Component::Normal(name) = part else {
            anyhow::bail!("unsafe notes directory path");
        };
        directory = Dir::from_std_file(directory.open_with(name, &options(true))?.into_std());
    }
    Ok(directory)
}

pub(super) fn rebuild(
    conn: &mut Connection,
    host: &Path,
    vault: Option<&Path>,
    limits: Limits,
    before_commit: impl FnOnce() -> Result<()>,
) -> Result<(u64, u64)> {
    let sources = sources(host, vault)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    super::db::reject_index_triggers(&tx)?;
    ddl(&tx)?;
    if vault.is_none() {
        for tag in ["vault_research", "vault_dashboards", "vault_runbooks"] {
            ensure!(
                !previous_source(&tx, tag)?,
                "previously indexed vault source {tag} requires a vault root"
            );
        }
    }
    let mut roots = Vec::new();
    for source in sources {
        let directory = if source.enabled {
            open_root(&source.root)?
        } else {
            None
        };
        ensure!(
            directory.is_some() || !previous_source(&tx, source.tag)?,
            "previously indexed notes source {} is missing: {}",
            source.tag,
            source.root.display()
        );
        if let Some(directory) = directory {
            roots.push((source, directory));
        }
    }
    tx.execute("DELETE FROM notes_fts", [])?;
    tx.execute("DELETE FROM note_tables", [])?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
    let mut counts = Counts {
        files: 0,
        tables: 0,
        examined: 0,
        bytes: 0,
    };
    for (source, directory) in roots {
        scan(&tx, &source, directory, limits, &mut counts, now)?;
    }
    before_commit()?;
    tx.commit()?;
    Ok((counts.files as u64, counts.tables))
}
