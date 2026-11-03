//! Durable append-only recovery journal for the native checkout transaction.
//! This is a prerequisite; it does not publish files, index or Git references.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
const JOURNAL_LIMIT: usize = 32 * 1024 * 1024;
const BLOB_LIMIT: usize = 64 * 1024 * 1024;
const RECORD_LIMIT: usize = 64 * 1024;
const COUNT_LIMIT: u64 = 100_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    pub device: u64,
    pub inode: u64,
    pub mode: u32,
    pub length: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Event {
    Begin {
        root: String,
        root_device: u64,
        root_inode: u64,
        common_dir: String,
        common_device: u64,
        common_inode: u64,
        index_path: String,
        index_parent_device: u64,
        index_parent_inode: u64,
        original_index: Option<Identity>,
        recovery_oid: String,
        branch: String,
        old: String,
        new: String,
        target: String,
        recovery_ref: String,
    },
    ObjectIntent {
        purpose: String,
    },
    BeforeSnapshot {
        reference: String,
        oid: String,
    },
    SnapshotReady {
        oid: String,
    },
    ForwardAcquire {
        subject: String,
    },
    ForwardHeld {
        subject: String,
    },
    ForwardRelease {
        subject: String,
    },
    ForwardReleased {
        subject: String,
    },
    BeforeDepot {
        name: String,
    },
    AfterDepot {
        name: String,
        device: u64,
        inode: u64,
    },
    BeforeStage {
        path: String,
        parent_device: u64,
        parent_inode: u64,
        slot: String,
        original: Option<Identity>,
        original_blob: Option<String>,
        has_staged: bool,
    },
    AfterStage {
        path: String,
        staged: Option<Identity>,
    },
    BeforePath {
        path: String,
        parent_device: u64,
        parent_inode: u64,
        slot: String,
        original: Option<Identity>,
        original_blob: Option<String>,
        staged: Option<Identity>,
    },
    AfterPath {
        path: String,
        published: Option<Identity>,
    },
    BeforeIndex {
        parent_device: u64,
        parent_inode: u64,
        original: Option<Identity>,
        original_blob: Option<String>,
        staged: Identity,
    },
    AfterIndex {
        published: Identity,
    },
    BeforeRef {
        branch: String,
        old: String,
        new: String,
    },
    AfterRef {
        branch: String,
        new: String,
    },
    GuardAcquire {
        subject: String,
    },
    GuardHeld {
        subject: String,
    },
    GuardRelease {
        subject: String,
    },
    GuardReleased {
        subject: String,
    },
    BeforeRollback {
        subject: String,
    },
    Rollback {
        subject: String,
        restored: bool,
        detail: String,
    },
    Complete,
    RecoveryComplete,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    seq: u64,
    event: Event,
}

pub(crate) struct Journal {
    dir: PathBuf,
    parent: PathBuf,
    directory_identity: (u64, u64),
    parent_identity: (u64, u64),
    file: File,
    file_identity: (u64, u64),
    sequence: u64,
    bytes: usize,
    blob_bytes: usize,
    blob_count: usize,
    poisoned: bool,
    validator: crate::checkout_sync_recovery::Validator,
}
fn verify_directory(path: &Path) -> Result<(u64, u64)> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o777 == 0o700,
        "recovery directory must be owned and mode 0700"
    );
    Ok((m.dev(), m.ino()))
}
impl Journal {
    /// Parent must be a pre-existing, operator-owned 0700 directory. An error
    /// after mkdir can leave recovery data at the returned diagnostic path.
    pub(crate) fn create(parent: &Path) -> Result<Self> {
        let parent = parent.canonicalize()?;
        let parent_identity = verify_directory(&parent)?;
        let dir = parent.join(crate::resource_id("checkout-sync-recovery")?);
        DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("create recovery directory {}", dir.display()))?;
        let build = (|| -> Result<_> {
            let directory_identity = verify_directory(&dir)?;
            let path = dir.join("journal.jsonl");
            let file = OpenOptions::new()
                .read(true)
                .append(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            let m = file.metadata()?;
            file.sync_all()?;
            File::open(&dir)?.sync_all()?;
            File::open(&parent)?.sync_all()?;
            Ok(Self {
                dir: dir.clone(),
                parent: parent.clone(),
                directory_identity,
                parent_identity,
                file,
                file_identity: (m.dev(), m.ino()),
                sequence: 0,
                bytes: 0,
                blob_bytes: 0,
                blob_count: 0,
                poisoned: false,
                validator: crate::checkout_sync_recovery::Validator::default(),
            })
        })();
        build.with_context(|| format!("recovery state retained at {}", dir.display()))
    }
    /// Reopen only while holding the exclusive recovery lease. Truncated or
    /// semantically invalid journals must never be resumed automatically.
    pub(crate) fn reopen(dir: &Path) -> Result<(Self, Vec<Event>)> {
        ensure!(
            !fs::symlink_metadata(dir)?.file_type().is_symlink(),
            "recovery path is a symlink"
        );
        let dir = dir.canonicalize()?;
        let parent = dir
            .parent()
            .context("recovery parent missing")?
            .to_path_buf();
        let parent_identity = verify_directory(&parent)?;
        let directory_identity = verify_directory(&dir)?;
        let events = read_records(&dir)?;
        let path = dir.join("journal.jsonl");
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.nlink() == 1 && metadata.len() <= JOURNAL_LIMIT as u64,
            "invalid recovery journal"
        );
        let mut blob_bytes = 0usize;
        let mut blob_count = 0usize;
        for item in fs::read_dir(&dir)? {
            let item = item?;
            let name = item.file_name();
            if name == "journal.jsonl" {
                continue;
            }
            let name = name.to_str().context("non-UTF-8 recovery file")?;
            ensure!(
                name.starts_with("blob-"),
                "unexpected recovery file; preserve directory"
            );
            let m = fs::symlink_metadata(item.path())?;
            ensure!(
                m.is_file()
                    && m.nlink() == 1
                    && m.len() <= BLOB_LIMIT as u64
                    && m.uid() == unsafe { libc::geteuid() }
                    && m.mode() & 0o777 == 0o600,
                "invalid recovery blob"
            );
            blob_count += 1;
            blob_bytes = blob_bytes
                .checked_add(m.len() as usize)
                .context("blob total overflow")?;
            ensure!(
                blob_count <= COUNT_LIMIT as usize && blob_bytes <= 2 * 1024 * 1024 * 1024,
                "recovery blob inventory exceeds limit"
            );
        }
        let mut validator = crate::checkout_sync_recovery::Validator::default();
        for event in &events {
            validator.push(event)?;
        }
        let journal = Self {
            dir: dir.clone(),
            parent,
            directory_identity,
            parent_identity,
            file,
            file_identity: (metadata.dev(), metadata.ino()),
            sequence: events.len() as u64,
            bytes: metadata.len() as usize,
            blob_bytes,
            blob_count,
            poisoned: false,
            validator,
        };
        journal.verify()?;
        ensure!(
            read_records(&dir)? == events,
            "recovery journal changed during reopen"
        );
        Ok((journal, events))
    }
    /// Read and authenticate a privately saved preimage; never follows a final symlink.
    pub(crate) fn read_blob(&self, slot: &str, expected_sha: &str) -> Result<Vec<u8>> {
        ensure!(
            !slot.is_empty()
                && slot.len() <= 128
                && slot
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
            "invalid recovery blob slot"
        );
        self.verify()?;
        let path = self.dir.join(format!("blob-{slot}"));
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)?;
        let before = file.metadata()?;
        ensure!(
            before.is_file()
                && before.nlink() == 1
                && before.len() <= BLOB_LIMIT as u64
                && before.uid() == unsafe { libc::geteuid() }
                && before.mode() & 0o777 == 0o600,
            "invalid recovery preimage"
        );
        let mut bytes = Vec::new();
        (&mut file)
            .take(BLOB_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        let named = fs::symlink_metadata(path)?;
        ensure!(
            bytes.len() <= BLOB_LIMIT
                && (
                    before.dev(),
                    before.ino(),
                    before.len(),
                    before.mtime(),
                    before.mtime_nsec(),
                    before.ctime(),
                    before.ctime_nsec()
                ) == (
                    after.dev(),
                    after.ino(),
                    after.len(),
                    after.mtime(),
                    after.mtime_nsec(),
                    after.ctime(),
                    after.ctime_nsec()
                )
                && (named.dev(), named.ino()) == (after.dev(), after.ino()),
            "recovery preimage changed"
        );
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == expected_sha,
            "recovery preimage digest mismatch"
        );
        self.verify()?;
        Ok(bytes)
    }
    pub(crate) fn path(&self) -> &Path {
        &self.dir
    }
    fn verify(&self) -> Result<()> {
        ensure!(
            verify_directory(&self.dir)? == self.directory_identity,
            "recovery directory changed"
        );
        let named = fs::symlink_metadata(self.dir.join("journal.jsonl"))?;
        let held = self.file.metadata()?;
        ensure!(
            held.is_file()
                && held.nlink() == 1
                && held.uid() == unsafe { libc::geteuid() }
                && held.mode() & 0o777 == 0o600
                && named.is_file()
                && named.nlink() == 1
                && (held.dev(), held.ino()) == self.file_identity
                && (named.dev(), named.ino()) == self.file_identity,
            "recovery journal replaced"
        );
        ensure!(
            verify_directory(&self.parent)? == self.parent_identity,
            "recovery parent changed or is unsafe"
        );
        Ok(())
    }
    /// An append error poisons this handle; never proceed to the planned mutation.
    pub(crate) fn append(&mut self, event: Event) -> Result<()> {
        ensure!(
            !self.poisoned && self.sequence < COUNT_LIMIT,
            "journal unavailable or full"
        );
        self.verify()?;
        ensure!(
            self.file.metadata()?.len() == self.bytes as u64,
            "recovery journal length changed"
        );
        let record = Record {
            seq: self.sequence,
            event,
        };
        let mut bytes = serde_json::to_vec(&record)?;
        ensure!(bytes.len() <= RECORD_LIMIT, "journal record too large");
        bytes.push(b'\n');
        ensure!(
            self.bytes
                .checked_add(bytes.len())
                .is_some_and(|n| n <= JOURNAL_LIMIT),
            "journal byte limit exceeded"
        );
        self.poisoned = true;
        self.validator.push(&record.event)?;
        self.file.write_all(&bytes)?;
        self.file.sync_all()?;
        self.verify()?;
        ensure!(
            self.file.metadata()?.len() == (self.bytes + bytes.len()) as u64,
            "recovery journal length changed"
        );
        self.bytes += bytes.len();
        self.sequence += 1;
        self.poisoned = false;
        Ok(())
    }
    /// Retain exact bounded input bytes before journalling any intent that uses
    /// them. Slot names are deterministic, private and cannot overwrite.
    pub(crate) fn save_blob(&mut self, slot: &str, bytes: &[u8]) -> Result<String> {
        ensure!(
            !self.poisoned
                && self.validator.permits_preimage()
                && bytes.len() <= BLOB_LIMIT
                && self.blob_count < COUNT_LIMIT as usize
                && self
                    .blob_bytes
                    .checked_add(bytes.len())
                    .is_some_and(|n| n <= 2 * 1024 * 1024 * 1024)
                && !slot.is_empty()
                && slot.len() <= 128
                && slot
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
            "invalid recovery blob slot or size"
        );
        self.verify()?;
        let path = self.dir.join(format!("blob-{slot}"));
        self.poisoned = true;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        let named = fs::symlink_metadata(&path)?;
        let held = file.metadata()?;
        ensure!(
            held.is_file()
                && held.nlink() == 1
                && (named.dev(), named.ino()) == (held.dev(), held.ino()),
            "recovery blob replaced"
        );
        File::open(&self.dir)?.sync_all()?;
        self.verify()?;
        self.blob_bytes += bytes.len();
        self.blob_count += 1;
        self.poisoned = false;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

/// Parse a complete journal only; truncated or reordered records require manual
/// recovery rather than interpreting a partial intent as committed state.
pub(crate) fn read_records(dir: &Path) -> Result<Vec<Event>> {
    verify_directory(dir)?;
    let path = dir.join("journal.jsonl");
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)?;
    let before = file.metadata()?;
    ensure!(
        before.is_file()
            && before.nlink() == 1
            && before.len() <= JOURNAL_LIMIT as u64
            && before.uid() == unsafe { libc::geteuid() }
            && before.mode() & 0o777 == 0o600,
        "invalid recovery journal file"
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take(JOURNAL_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    ensure!(
        (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec()
        ) == (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec()
        ),
        "recovery journal changed while reading"
    );
    let named = fs::symlink_metadata(&path)?;
    ensure!(
        (named.dev(), named.ino()) == (after.dev(), after.ino()) && named.is_file(),
        "recovery journal path changed"
    );
    ensure!(
        bytes.is_empty() || bytes.last() == Some(&b'\n'),
        "truncated recovery journal"
    );
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut events = Vec::new();
    for line in bytes[..bytes.len() - 1].split(|b| *b == b'\n') {
        ensure!(!line.is_empty(), "empty recovery journal record");
        ensure!(
            line.len() <= RECORD_LIMIT && events.len() < COUNT_LIMIT as usize,
            "recovery record limit"
        );
        let record: Record = serde_json::from_slice(line)?;
        ensure!(
            record.seq == events.len() as u64,
            "recovery journal sequence mismatch"
        );
        events.push(record.event);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn begin() -> Event {
        Event::Begin {
            root: "/repo".into(),
            root_device: 1,
            root_inode: 2,
            common_dir: "/repo/.git".into(),
            common_device: 1,
            common_inode: 3,
            index_path: "/repo/.git/index".into(),
            index_parent_device: 1,
            index_parent_inode: 3,
            original_index: None,
            recovery_oid: "1".repeat(40),
            branch: "refs/heads/main".into(),
            old: "1".repeat(40),
            new: "2".repeat(40),
            target: "refs/remotes/origin/main".into(),
            recovery_ref: "refs/snapshots/forge/test".into(),
        }
    }
    #[test]
    fn records_and_preimages_are_durable_and_ordered() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let mut journal = Journal::create(root.path()).unwrap();
        let mut initial = begin();
        if let Event::Begin { original_index, .. } = &mut initial {
            *original_index = Some(Identity {
                device: 1,
                inode: 1,
                mode: 0o100600,
                length: 3,
                sha256: format!("{:x}", Sha256::digest(b"old")),
            });
        }
        journal.append(initial).unwrap();
        assert_eq!(
            journal.save_blob("index", b"old").unwrap(),
            format!("{:x}", Sha256::digest(b"old"))
        );
        journal
            .append(Event::BeforeIndex {
                parent_device: 1,
                parent_inode: 3,
                original: Some(Identity {
                    device: 1,
                    inode: 1,
                    mode: 0o100600,
                    length: 3,
                    sha256: format!("{:x}", Sha256::digest(b"old")),
                }),
                original_blob: Some("index".into()),
                staged: Identity {
                    device: 1,
                    inode: 2,
                    mode: 0o100600,
                    length: 3,
                    sha256: "a".repeat(64),
                },
            })
            .unwrap();
        assert!(matches!(
            read_records(journal.path()).unwrap().as_slice(),
            [Event::Begin { .. }, Event::BeforeIndex { .. }]
        ));
        assert!(journal.save_blob("index", b"new").is_err());
    }
    #[test]
    fn truncated_or_replaced_journal_never_looks_complete() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let mut journal = Journal::create(root.path()).unwrap();
        journal.append(begin()).unwrap();
        let path = journal.path().join("journal.jsonl");
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{")
            .unwrap();
        assert!(read_records(journal.path()).is_err());
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"").unwrap();
        assert!(journal.append(Event::Complete).is_err());
    }
}
