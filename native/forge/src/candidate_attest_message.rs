//! Fresh coordinator-owned commit message binding; checksums are not approval.
use super::*;
use crate::{candidate_execution_plan::SelectionArgs, candidate_review_lock::ReviewLock};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};
const MAX_MESSAGE: usize = 1024 * 1024;
const NAMES: [&str; 3] = ["Governance-Tree", "Governance-Policy", "Governance-Receipt"];

pub(super) struct Message {
    path: PathBuf,
    file: File,
    before: std::fs::Metadata,
    bytes: Vec<u8>,
}
fn same(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.is_file()
        && b.is_file()
        && a.nlink() == 1
        && b.nlink() == 1
        && a.uid() == unsafe { libc::geteuid() }
        && (
            a.dev(),
            a.ino(),
            a.len(),
            a.mtime(),
            a.mtime_nsec(),
            a.ctime(),
            a.ctime_nsec(),
        ) == (
            b.dev(),
            b.ino(),
            b.len(),
            b.mtime(),
            b.mtime_nsec(),
            b.ctime(),
            b.ctime_nsec(),
        )
}
impl Message {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        ensure!(
            path.as_os_str().as_encoded_bytes().len() <= 4096
                && path
                    .components()
                    .all(|p| matches!(p, Component::RootDir | Component::Normal(_))),
            "message path must be absolute and normalized"
        );
        ensure!(
            path.canonicalize()? == path,
            "message path must not contain symlinks"
        );
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)?;
        let before = file.metadata()?;
        ensure!(
            before.is_file()
                && before.nlink() == 1
                && before.uid() == unsafe { libc::geteuid() }
                && before.len() <= MAX_MESSAGE as u64,
            "message must be owned, regular, singly linked and at most 1 MiB"
        );
        let mut bytes = Vec::new();
        (&mut file)
            .take((MAX_MESSAGE + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_MESSAGE && !bytes.contains(&0),
            "message exceeds limit or contains NUL"
        );
        std::str::from_utf8(&bytes).context("message must be UTF-8")?;
        let result = Self {
            path,
            file,
            before,
            bytes,
        };
        result.verify()?;
        Ok(result)
    }
    fn verify(&self) -> Result<()> {
        ensure!(
            same(&self.before, &self.file.metadata()?)
                && same(&self.before, &fs::symlink_metadata(&self.path)?)
                && self.path.canonicalize()? == self.path,
            "message changed during review"
        );
        Ok(())
    }
}
pub(super) struct Completed {
    bytes: Vec<u8>,
    tree: String,
    policy: String,
    digest: String,
}
impl Completed {
    pub(super) fn new(
        report: &Value,
        selection: &Selection,
        plans: &[Arc<CheckPlan>],
        ids: &[String],
    ) -> Result<Self> {
        ensure!(
            report["executed"] == true
                && report["review_complete"] == true
                && report["execution_verified"] == true
                && report["decision"] == "pass"
                && report["approval"] == false,
            "attestation requires a fresh complete passing review"
        );
        let checks = report["checks"]
            .as_array()
            .context("review checks missing")?;
        ensure!(
            checks.len() == ids.len(),
            "review check inventory incomplete"
        );
        for (id, row) in ids.iter().zip(checks) {
            ensure!(
                row["check_id"] == id.as_str(),
                "review check identity differs"
            );
            if row["status"] == "skipped" {
                ensure!(
                    plans.iter().any(|p| &p.id == id && !p.scheduled),
                    "required check skipped"
                );
            } else {
                ensure!(row["status"] == "passed", "required check did not pass");
            }
        }
        let bytes = serde_json::to_vec(report)?;
        let receipt = crate::candidate_receipt::parse_and_verify(&bytes)?;
        ensure!(
            receipt.decision() == "pass"
                && report["candidate"]["tree_oid"] == selection.candidate.tree_oid
                && report["policy"]["sha256"] == selection.policy.digest,
            "review binding changed"
        );
        Ok(Self {
            bytes,
            tree: selection.candidate.tree_oid.clone(),
            policy: selection.policy.digest.clone(),
            digest: receipt.digest().to_owned(),
        })
    }
}
fn revalidate(args: &SelectionArgs, selection: &Selection, lock: &ReviewLock) -> Result<()> {
    lock.verify()?;
    let current = Selection::load(args)?;
    ensure!(
        current.candidate.tree_oid == selection.candidate.tree_oid
            && current.candidate.base_tree_oid == selection.candidate.base_tree_oid
            && current.policy.digest == selection.policy.digest
            && current.grants.digest() == selection.grants.digest(),
        "index, policy or operator grants changed during review"
    );
    lock.verify()
}
fn directory(selection: &Selection) -> Result<PathBuf> {
    let mut dir = selection.source()?.common_dir().to_owned();
    for name in ["governance", "receipts", "pre-commit"] {
        dir.push(name);
        match fs::symlink_metadata(&dir) {
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match fs::DirBuilder::new().mode(0o700).create(&dir) {
                    Ok(()) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(e) => return Err(e.into()),
                }
            }
            Err(e) => return Err(e.into()),
        }
        let meta = fs::symlink_metadata(&dir)?;
        ensure!(
            meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o002 == 0,
            "unsafe canonical receipt directory"
        );
    }
    Ok(dir)
}
fn trailers(original: &[u8]) -> Result<[Option<&str>; 3]> {
    let text = std::str::from_utf8(original)?;
    let mut found: [Option<&str>; 3] = [None, None, None];
    for line in text.lines() {
        for (i, name) in NAMES.iter().enumerate() {
            if let Some(value) = line.strip_prefix(name).and_then(|s| s.strip_prefix(':')) {
                ensure!(found[i].is_none(), "duplicate governance trailer");
                found[i] = Some(value.trim());
            }
        }
    }
    Ok(found)
}
fn append(original: &[u8], completed: &Completed) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(original)?;
    let expected = [
        completed.tree.as_str(),
        completed.policy.as_str(),
        completed.digest.as_str(),
    ];
    let suffix = if text.ends_with("\n\n") {
        ""
    } else if text.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let mut out = text.to_owned();
    out.push_str(suffix);
    for (name, value) in NAMES.iter().zip(expected) {
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push('\n');
    }
    ensure!(out.len() <= MAX_MESSAGE, "attested message exceeds 1 MiB");
    Ok(out.into_bytes())
}
fn previous_receipt(
    path: &Path,
    found: [Option<&str>; 3],
    completed: &Completed,
) -> Result<(File, std::fs::Metadata)> {
    ensure!(
        found[0] == Some(completed.tree.as_str()) && found[1] == Some(completed.policy.as_str()),
        "partial or conflicting governance trailers"
    );
    let digest = found[2].context("partial governance trailers")?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .context("existing trailers require the matching canonical receipt")?;
    let before = file.metadata()?;
    ensure!(
        before.is_file()
            && before.nlink() == 1
            && before.uid() == unsafe { libc::geteuid() }
            && before.len() <= crate::candidate_receipt::MAX_BYTES as u64,
        "existing canonical receipt is unsafe"
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take((crate::candidate_receipt::MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= crate::candidate_receipt::MAX_BYTES
            && same(&before, &file.metadata()?)
            && same(&before, &fs::symlink_metadata(path)?),
        "existing canonical receipt changed"
    );
    let receipt = crate::candidate_receipt::parse_and_verify(&bytes)?;
    ensure!(
        receipt.digest() == digest
            && receipt.decision() == "pass"
            && receipt.payload()["schema_version"] == 1
            && receipt.payload()["surface"] == "pre-commit"
            && receipt.payload()["profile"] == "fast"
            && receipt.payload()["review_complete"] == true
            && receipt.payload()["execution_verified"] == true
            && receipt.payload()["approval"] == false
            && receipt.payload()["candidate"]["tree_oid"] == completed.tree
            && receipt.payload()["policy"]["sha256"] == completed.policy,
        "existing canonical receipt conflicts with governance trailers"
    );
    Ok((file, before))
}
pub(super) fn publish(
    completed: Completed,
    message: Message,
    args: &SelectionArgs,
    selection: &Selection,
    lock: &ReviewLock,
) -> Result<Value> {
    message.verify()?;
    revalidate(args, selection, lock)?;
    let path = directory(selection)?.join(format!("{}-fast.json", completed.tree));
    ensure!(
        message.path != path,
        "message path must differ from canonical receipt"
    );
    let found = trailers(&message.bytes)?;
    if found.iter().any(Option::is_some) {
        let existing_digest = found[2].context("partial governance trailers")?.to_owned();
        let (old, before) = previous_receipt(&path, found, &completed)?;
        revalidate(args, selection, lock)?;
        message.verify()?;
        ensure!(
            same(&before, &old.metadata()?) && same(&before, &fs::symlink_metadata(&path)?),
            "existing canonical receipt changed"
        );
        lock.verify()?;
        return Ok(
            json!({"canonical_receipt":path,"receipt_digest":existing_digest,"reused":true}),
        );
    }
    let content = append(&message.bytes, &completed)?;
    crate::candidate_report_store::publish(&path, &completed.bytes)
        .context("canonical receipt publication failed")?;
    revalidate(args, selection, lock)
        .context("canonical receipt published; review binding changed before message update")?;
    message
        .verify()
        .context("canonical receipt published; message changed before update")?;
    crate::candidate_report_store::publish(&message.path, &content)
        .context("canonical receipt published; message update failed")?;
    lock.verify()
        .context("receipt and message may be published; governance lock verification failed")?;
    Ok(json!({"canonical_receipt":path,"receipt_digest":completed.digest,"reused":false}))
}
#[cfg(test)]
#[path = "candidate_attest_message_tests.rs"]
mod tests;
