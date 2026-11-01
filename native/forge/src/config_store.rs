//! Bounded configuration snapshots and durable publication. Cooperating writers
//! share a permanent lock; operator-owned parent directories and same-UID writers
//! are trusted. Dry runs never create a directory, lock or backup.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(super) const CONFIG_LIMIT: usize = 1024 * 1024;
const BACKUP_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileState {
    pub text: String,
    pub mode: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Backup {
    pub schema_version: u32,
    pub target: PathBuf,
    pub before: Option<FileState>,
    pub after: Option<FileState>,
}

fn validate_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o022 == 0,
        "configuration directory must be owned, nonsymlink and not writable by others: {}",
        path.display()
    );
    Ok(())
}

pub(super) fn parents(host: &Path, relative: &Path, create: bool) -> Result<()> {
    validate_directory(host)?;
    let mut path = host.to_path_buf();
    for part in relative
        .parent()
        .context("configuration needs parent")?
        .components()
    {
        ensure!(
            matches!(part, std::path::Component::Normal(_)),
            "configuration parent must be relative"
        );
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(_) => validate_directory(&path)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    return Ok(());
                }
                match fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => {
                        File::open(path.parent().context("new directory parent")?)?.sync_all()?
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(e) => return Err(e.into()),
                }
                validate_directory(&path)?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn validate_file(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o7022 == 0,
        "configuration file must be owned, regular, singly linked and not writable by others"
    );
    Ok(())
}

fn read_limit(path: &Path, limit: usize) -> Result<Option<FileState>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    validate_file(&file)?;
    let meta = file.metadata()?;
    ensure!(
        meta.len() <= limit as u64,
        "configuration file exceeds {limit} bytes"
    );
    let mut text = String::new();
    file.take(limit as u64 + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() <= limit,
        "configuration file exceeds {limit} bytes"
    );
    Ok(Some(FileState {
        text,
        mode: meta.mode() & 0o777,
    }))
}

pub(super) fn read(path: &Path) -> Result<Option<FileState>> {
    read_limit(path, CONFIG_LIMIT)
}

pub(super) fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".forge-native.bak.json");
    PathBuf::from(name)
}

pub(super) fn read_backup(path: &Path) -> Result<Backup> {
    let state =
        read_limit(&backup_path(path), BACKUP_LIMIT)?.context("native provider backup missing")?;
    ensure!(state.mode & 0o077 == 0, "provider backup must be private");
    let backup: Backup = serde_json::from_str(&state.text)?;
    ensure!(
        backup.schema_version == 1 && backup.target == path,
        "provider backup target or schema mismatch"
    );
    for state in [&backup.before, &backup.after].into_iter().flatten() {
        ensure!(
            state.text.len() <= CONFIG_LIMIT && state.mode & !0o755 == 0,
            "provider backup has invalid content size or permissions"
        );
    }
    Ok(backup)
}

pub(super) fn lock(path: &Path) -> Result<forge::file_lock::Exclusive> {
    lock_named(path, "provider")
}

pub(super) fn lock_named(path: &Path, purpose: &str) -> Result<forge::file_lock::Exclusive> {
    ensure!(
        !purpose.is_empty() && purpose.bytes().all(|b| b.is_ascii_lowercase()),
        "invalid configuration lock purpose"
    );
    let lock_path = path.with_file_name(format!(".forge-{purpose}.lock"));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(lock_path)?;
    validate_file(&file)?;
    ensure!(
        file.metadata()?.mode() & 0o077 == 0,
        "provider lock must be private"
    );
    forge::file_lock::Exclusive::try_new(file, "configuration writer")
}

fn publish(path: &Path, state: &FileState) -> Result<()> {
    let parent = path.parent().context("configuration parent")?;
    let temporary = parent.join(forge::resource_id("provider-pending")?);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        file.write_all(state.text.as_bytes())?;
        file.set_permissions(fs::Permissions::from_mode(state.mode))?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?
            .sync_all()
            .context("configuration published but directory sync failed")?;
        Ok(())
    })();
    if result.is_err() {
        if let Err(e) = fs::remove_file(&temporary) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("forge: provider staging cleanup: {e}");
            }
        }
    }
    result
}

pub(super) fn apply(
    path: &Path,
    before: &Option<FileState>,
    after: &Option<FileState>,
) -> Result<()> {
    ensure!(
        &read(path)? == before,
        "provider configuration changed since planning; retry"
    );
    // Refuse dangerous backup targets before publishing anything. This check and
    // the target comparison are protected against cooperating writers by lock().
    let backup_path = backup_path(path);
    if let Some(state) = read_limit(&backup_path, BACKUP_LIMIT)? {
        ensure!(state.mode & 0o077 == 0, "provider backup must be private");
    }
    let backup = Backup {
        schema_version: 1,
        target: path.to_owned(),
        before: before.clone(),
        after: after.clone(),
    };
    let text = serde_json::to_string(&backup)?;
    ensure!(text.len() <= BACKUP_LIMIT, "provider backup exceeds 16 MiB");
    publish(&backup_path, &FileState { text, mode: 0o600 })?;
    restore(path, after)
}

pub(super) fn restore(path: &Path, state: &Option<FileState>) -> Result<()> {
    if let Some(state) = state {
        publish(path, state)?;
    } else {
        match fs::remove_file(path) {
            Ok(()) => File::open(path.parent().context("configuration parent")?)?.sync_all()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
