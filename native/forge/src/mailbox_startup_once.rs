//! A bounded, durable success set under one permanent nonblocking lock. Lock
//! files are never pruned: unlinking a live flock file allows concurrent owners.
//! Operator-owned parents and unconfined processes running as this UID are trusted.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: usize = 512 * 1024;
const MAX_SESSIONS: usize = 4096;
const TTL: u64 = 86400;

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Done {
    sessions: BTreeMap<String, u64>,
}

pub(super) struct State {
    _lock: forge::file_lock::Exclusive,
    directory: PathBuf,
    entries: Done,
    key: String,
    now: u64,
}

fn private_directory(path: &Path, create: bool) -> Result<()> {
    if create {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => File::open(path.parent().context("startup state parent")?)?.sync_all()?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
    }
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "startup state directory must be private and owned"
    );
    Ok(())
}

fn private_file(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o077 == 0,
        "startup state file must be private, owned and singly linked"
    );
    Ok(file)
}

fn load(path: &Path, now: u64) -> Result<Done> {
    let file = match private_file(path, false) {
        Ok(file) => file,
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(Done::default())
        }
        Err(e) => return Err(e),
    };
    ensure!(
        file.metadata()?.len() <= MAX_BYTES as u64,
        "startup state exceeds 512 KiB"
    );
    let mut bytes = String::new();
    file.take(MAX_BYTES as u64 + 1).read_to_string(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BYTES, "startup state exceeds 512 KiB");
    let mut entries: Done = serde_json::from_value(forge::tool_schema::parse_document(&bytes)?)?;
    ensure!(
        entries.sessions.len() <= MAX_SESSIONS,
        "startup state exceeds 4096 sessions"
    );
    for (key, timestamp) in &entries.sessions {
        ensure!(
            key.len() == 64
                && key
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "invalid session digest"
        );
        ensure!(
            *timestamp <= now,
            "startup clock moved backwards; retry after recorded time"
        );
    }
    entries
        .sessions
        .retain(|_, timestamp| now - *timestamp < TTL);
    Ok(entries)
}

impl State {
    pub(super) fn open(identity_dir: &Path, provider: &str, session: &str) -> Result<Self> {
        ensure!(
            !session.trim().is_empty()
                && session.len() <= 512
                && !session.chars().any(char::is_control),
            "session_id must be 1..512 UTF-8 bytes without control characters"
        );
        private_directory(identity_dir, true)?;
        let directory = identity_dir.join(".session-start");
        private_directory(&directory, true)?;
        let lock = private_file(&directory.join("native.lock"), true)?;
        let lock = forge::file_lock::Exclusive::try_new(lock, "startup session state")?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let entries = load(&directory.join("native.json"), now)?;
        let mut hash = Sha256::new();
        hash.update(provider.as_bytes());
        hash.update([0]);
        hash.update(session.as_bytes());
        let key = format!("{:x}", hash.finalize());
        ensure!(
            entries.sessions.contains_key(&key) || entries.sessions.len() < MAX_SESSIONS,
            "startup state has 4096 active sessions; retry after expiry"
        );
        Ok(Self {
            _lock: lock,
            directory,
            entries,
            key,
            now,
        })
    }

    pub(super) fn already_done(&self) -> bool {
        self.entries.sessions.contains_key(&self.key)
    }

    pub(super) fn commit(&mut self) -> Result<()> {
        self.entries.sessions.insert(self.key.clone(), self.now);
        let bytes = serde_json::to_vec(&self.entries)?;
        ensure!(bytes.len() <= MAX_BYTES, "startup state exceeds 512 KiB");
        let temporary = self.directory.join(forge::resource_id("startup-pending")?);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let result = (|| -> Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, self.directory.join("native.json"))?;
            File::open(&self.directory)?
                .sync_all()
                .context("startup state published but directory sync failed")?;
            Ok(())
        })();
        if result.is_err() {
            if let Err(error) = fs::remove_file(&temporary) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("forge: startup state cleanup: {error}");
                }
            }
        }
        result
    }
}

#[cfg(test)]
#[path = "mailbox_startup_once_tests.rs"]
mod tests;
