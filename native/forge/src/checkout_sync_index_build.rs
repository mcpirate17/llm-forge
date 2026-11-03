//! Construct a replacement Git index in the private recovery directory.
use crate::{
    candidate_source::Source, checkout_sync_admission::PlannedPath, checkout_sync_reconcile::Entry,
};
use anyhow::{ensure, Context, Result};
use git2::{Index, IndexEntry, IndexTime};
use sha1::{Digest, Sha1};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

/// `changes` contain canonical Git bytes, never raw worktree bytes. The caller
/// records immutable object creation intent before calling this function.
pub(crate) fn build(
    source: &Source,
    recovery: &Path,
    original: Option<&[u8]>,
    changes: &[PlannedPath],
) -> Result<Vec<u8>> {
    // libgit2 validates the checksum and TREE payload. Drop TREE only after
    // parsing: it is a reconstructible optimization, and retaining a stale
    // cache would make subsequent write-tree trust obsolete object IDs.
    let clean = match original {
        Some(bytes) => {
            let _ = crate::candidate_source::parse_index(bytes)?;
            Some(strip_tree_extension(bytes)?)
        }
        None => None,
    };
    let path = recovery.join(crate::resource_id("checkout-sync-index")?);
    if let Some(bytes) = clean.as_deref() {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    let mut index = Index::open(&path)?;
    for change in changes {
        let path = Path::new(&change.path);
        match &change.index {
            None => {
                if index.get_path(path, 0).is_some() {
                    index.remove_path(path)?;
                }
            }
            Some(entry) => {
                let (mode, bytes) = match entry {
                    Entry::File { executable, bytes } => {
                        (if *executable { 0o100755 } else { 0o100644 }, bytes)
                    }
                    Entry::Symlink(bytes) => (0o120000, bytes),
                };
                let oid = source.repository().blob(bytes)?;
                let mut item = index.get_path(path, 0).unwrap_or_else(|| IndexEntry {
                    ctime: IndexTime::new(0, 0),
                    mtime: IndexTime::new(0, 0),
                    dev: 0,
                    ino: 0,
                    mode,
                    uid: 0,
                    gid: 0,
                    file_size: 0,
                    id: oid,
                    flags: 0,
                    flags_extended: 0,
                    path: change.path.as_bytes().to_vec(),
                });
                item.mode = mode;
                item.id = oid;
                // Git's stat cache must not make an untouched worktree inode
                // appear equal to a newly staged, same-size canonical blob.
                item.ctime = IndexTime::new(0, 0);
                item.mtime = IndexTime::new(0, 0);
                item.dev = 0;
                item.ino = 0;
                item.uid = 0;
                item.gid = 0;
                item.file_size = 0;
                index.add(&item)?;
            }
        }
    }
    index.write()?;
    drop(index);
    read_and_remove_scratch(&path)
}
fn read_and_remove_scratch(path: &Path) -> Result<Vec<u8>> {
    let before = fs::symlink_metadata(path)?;
    let bytes = crate::bounded_file::read(path, 64 * 1024 * 1024)?;
    let after = fs::symlink_metadata(path)?;
    let stamp = |m: &fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.mode(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    ensure!(
        before.is_file() && before.nlink() == 1 && stamp(&before) == stamp(&after),
        "private replacement index was replaced"
    );
    // This scratch file is not a recovery preimage. Remove it before any live
    // path publication so the journal's strict recovery inventory stays valid.
    // An interrupted build retains artifacts and precedes all path mutations.
    fs::remove_file(path)?;
    fs::File::open(path.parent().context("scratch index parent missing")?)?.sync_all()?;
    Ok(bytes)
}
/// Parse the bounded extension framing; TREE is cache only and is removed
/// before rewriting. All other extensions refuse, including unknown optional
/// extensions, because their semantic round-trip has not been established.
pub(crate) fn strip_tree_extension(raw: &[u8]) -> Result<Vec<u8>> {
    ensure!(raw.len() >= 32 && &raw[..4] == b"DIRC", "invalid Git index");
    let version = u32::from_be_bytes(raw[4..8].try_into()?);
    let count = u32::from_be_bytes(raw[8..12].try_into()?) as usize;
    ensure!(
        (2..=4).contains(&version) && count <= 100_000,
        "unsupported index version or count"
    );
    let end = raw.len() - 20;
    let mut cursor = 12usize;
    for _ in 0..count {
        let start = cursor;
        ensure!(
            cursor.checked_add(62).is_some_and(|p| p <= end),
            "truncated index entry"
        );
        let extended = raw[cursor + 60] & 0x40 != 0;
        cursor += 62 + if extended { 2 } else { 0 };
        ensure!(cursor <= end, "truncated index flags");
        if version == 4 {
            loop {
                let byte = *raw.get(cursor).context("truncated index path prefix")?;
                cursor += 1;
                if byte & 0x80 == 0 {
                    break;
                }
                ensure!(cursor <= end, "truncated index prefix");
            }
        }
        let n = raw
            .get(cursor..end)
            .context("invalid index offset")?
            .iter()
            .position(|b| *b == 0)
            .context("unterminated index path")?;
        cursor += n + 1;
        if version != 4 {
            cursor = start
                .checked_add((cursor - start + 7) & !7)
                .context("index offset overflow")?;
        }
        ensure!(cursor <= end, "truncated index entry");
    }
    let entries_end = cursor;
    let mut saw_tree = false;
    while cursor < end {
        ensure!(end - cursor >= 8, "truncated Git index extension header");
        let tag = &raw[cursor..cursor + 4];
        let size = u32::from_be_bytes(raw[cursor + 4..cursor + 8].try_into()?) as usize;
        cursor += 8;
        ensure!(
            cursor.checked_add(size).is_some_and(|n| n <= end),
            "truncated Git index extension payload"
        );
        ensure!(
            tag == b"TREE",
            "unsupported Git index extension {:?}; index preserved",
            String::from_utf8_lossy(tag)
        );
        ensure!(!saw_tree, "duplicate TREE index extension");
        saw_tree = true;
        cursor += size;
    }
    if !saw_tree {
        return Ok(raw.to_vec());
    }
    let mut stripped = raw[..entries_end].to_vec();
    let checksum = Sha1::digest(&stripped);
    stripped.extend_from_slice(&checksum);
    Ok(stripped)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::MetadataExt, process::Command};
    fn git(repo: &Path, index: Option<&Path>, args: &[&str]) -> Vec<u8> {
        let mut command = Command::new("git");
        command
            .current_dir(repo)
            .args(args)
            .env_remove("GIT_EXTERNAL_DIFF");
        if let Some(path) = index {
            command.env("GIT_INDEX_FILE", path);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    #[test]
    fn tree_cache_is_rebuilt_and_same_size_staged_blob_does_not_hide_unstaged_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let work = dir.path().join("a.txt");
        fs::write(&work, b"alpha\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        drop(index);
        let _ = git(dir.path(), None, &["write-tree"]);
        let raw = fs::read(repo.path().join("index")).unwrap();
        let without_cache = strip_tree_extension(&raw).unwrap();
        assert!(
            without_cache.len() < raw.len(),
            "fixture must contain a Git-generated TREE extension"
        );
        assert!(git(
            dir.path(),
            None,
            &["diff", "--no-ext-diff", "--name-only", "--", "a.txt"]
        )
        .is_empty());
        let before = fs::metadata(&work).unwrap();
        let source = Source::open(dir.path()).unwrap();
        let changes = [PlannedPath {
            path: "a.txt".into(),
            index: Some(Entry::File {
                executable: false,
                bytes: b"bravo\n".to_vec(),
            }),
            worktree: None,
        }];
        let result = build(&source, scratch.path(), Some(&raw), &changes).unwrap();
        let stripped = strip_tree_extension(&result).unwrap();
        assert_eq!(
            stripped.len(),
            result.len(),
            "stale TREE cache must not survive index rewrite"
        );
        let parsed = crate::candidate_source::parse_index(&result).unwrap();
        let entry = parsed.get_path(Path::new("a.txt"), 0).unwrap();
        assert_eq!(
            (entry.dev, entry.ino, entry.uid, entry.gid, entry.file_size),
            (0, 0, 0, 0, 0)
        );
        assert_eq!((entry.ctime.seconds(), entry.mtime.seconds()), (0, 0));
        let replacement = scratch.path().join("published-index");
        fs::write(&replacement, &result).unwrap();
        let after = fs::metadata(&work).unwrap();
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.mtime(),
                before.mtime_nsec(),
                before.len()
            ),
            (
                after.dev(),
                after.ino(),
                after.mtime(),
                after.mtime_nsec(),
                after.len()
            )
        );
        assert_eq!(fs::read(&work).unwrap(), b"alpha\n");
        let diff = git(
            dir.path(),
            Some(&replacement),
            &["diff", "--no-ext-diff", "--name-only", "--", "a.txt"],
        );
        assert_eq!(
            diff, b"a.txt\n",
            "Git must observe the unchanged worktree as unstaged"
        );
        let mut expected = repo.treebuilder(None).unwrap();
        expected
            .insert("a.txt", repo.blob(b"bravo\n").unwrap(), 0o100644)
            .unwrap();
        let expected = expected.write().unwrap().to_string();
        let observed = git(dir.path(), Some(&replacement), &["write-tree"]);
        assert_eq!(std::str::from_utf8(&observed).unwrap().trim(), expected);
    }
    #[test]
    fn malformed_tree_cache_is_not_silently_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("a"), b"data").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a")).unwrap();
        index.write().unwrap();
        drop(index);
        let _ = git(dir.path(), None, &["write-tree"]);
        let raw = fs::read(repo.path().join("index")).unwrap();
        let stripped = strip_tree_extension(&raw).unwrap();
        assert!(stripped.len() < raw.len());
        let mut malformed = stripped[..stripped.len() - 20].to_vec();
        malformed.extend_from_slice(b"TREE\0\0\0\0");
        let checksum = Sha1::digest(&malformed);
        malformed.extend_from_slice(&checksum);
        let source = Source::open(dir.path()).unwrap();
        assert!(build(&source, scratch.path(), Some(&malformed), &[]).is_err());
    }
    #[test]
    fn unknown_index_extension_is_refused_without_rewriting_it() {
        let mut raw = b"DIRC\0\0\0\x02\0\0\0\0".to_vec();
        raw.extend_from_slice(b"TEST\0\0\0\0");
        raw.extend_from_slice(&[0; 20]);
        assert!(strip_tree_extension(&raw).is_err());
        let mut truncated = b"DIRC\0\0\0\x02\0\0\0\0TREE\0\0\0\x10".to_vec();
        truncated.extend_from_slice(&[0; 20]);
        assert!(strip_tree_extension(&truncated).is_err());
        let mut duplicate = b"DIRC\0\0\0\x02\0\0\0\0TREE\0\0\0\0TREE\0\0\0\0".to_vec();
        duplicate.extend_from_slice(&[0; 20]);
        assert!(strip_tree_extension(&duplicate).is_err());
        raw.truncate(12);
        raw.extend_from_slice(&[0; 20]);
        assert!(strip_tree_extension(&raw).is_ok());
    }
}
