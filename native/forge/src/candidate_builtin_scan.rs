//! Snapshot scan payload preparation. No lossy decoding or source text in errors.
use super::*;
use crate::candidate_assessment::{Finding, Severity};
use cap_std::fs::{Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
use std::{
    collections::BTreeMap,
    io::{self, Read},
};
const MAX_FILE: usize = 1024 * 1024;
const MAX_FILES: usize = 100_000;
const CHUNK: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct Ledger {
    expected: usize,
    read: usize,
    skipped: BTreeMap<String, String>,
}
struct Reads {
    reserved: usize,
}
impl Reads {
    fn reserve(&mut self, bytes: u64) -> Result<()> {
        // Reserve the advertised read plus one growth-detection byte. Failed
        // UTF8 attempts also consume this aggregate admission credit.
        self.reserved = self
            .reserved
            .checked_add(usize::try_from(bytes)?)
            .and_then(|n| n.checked_add(1))
            .context("scan read budget overflow")?;
        ensure!(
            self.reserved <= MAX_INPUT_BYTES,
            "scan attempted reads exceed 32 MiB"
        );
        Ok(())
    }
}
enum Text {
    Included(String),
    Skipped,
    Unreadable(String),
}
fn io_reason(error: &io::Error) -> String {
    // Never interpolate an arbitrary error Display: it may contain source text
    // from a lower layer. Kind and numeric errno are finite native diagnostics.
    format!(
        "NativeReadError: kind={:?}, errno={:?}",
        error.kind(),
        error.raw_os_error()
    )
}
fn unreadable<T>(result: io::Result<T>) -> std::result::Result<T, Text> {
    result.map_err(|error| Text::Unreadable(io_reason(&error)))
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    a.is_file()
        && b.is_file()
        && a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn normalize(text: String) -> String {
    if !text.contains('\r') {
        return text;
    }
    let mut output = String::with_capacity(text.len());
    for (index, part) in text.split('\r').enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(if index > 0 {
            part.strip_prefix('\n').unwrap_or(part)
        } else {
            part
        });
    }
    output
}
fn read_bytes(
    file: &mut cap_std::fs::File,
    size: usize,
    cancel: Option<&Cancellation>,
) -> Result<Vec<u8>> {
    let mut raw = Vec::with_capacity(size);
    let mut buffer = [0u8; CHUNK];
    for _ in 0..=MAX_FILE / CHUNK {
        live(cancel)?;
        let room = (size + 1 - raw.len()).min(CHUNK);
        let count = file.read(&mut buffer[..room])?;
        if count == 0 {
            ensure!(raw.len() == size, "scan candidate file shrank during read");
            return Ok(raw);
        }
        ensure!(
            raw.len() + count <= size,
            "scan candidate file grew during read"
        );
        raw.extend_from_slice(&buffer[..count]);
    }
    anyhow::bail!("scan read iteration bound exceeded")
}
fn read_text(
    directory: &Dir,
    path: &str,
    secret: bool,
    policy_limit: u64,
    reads: &mut Reads,
    cancel: Option<&Cancellation>,
) -> Result<Text> {
    live(cancel)?;
    let metadata = if secret {
        directory.symlink_metadata(path)
    } else {
        directory.metadata(path)
    };
    let before = match metadata {
        Ok(before) => before,
        Err(error) => return Ok(Text::Unreadable(io_reason(&error))),
    };
    if secret && (before.is_symlink() || !before.is_file() || before.len() > policy_limit) {
        return Ok(Text::Skipped);
    }
    if !before.is_file() {
        return Ok(Text::Unreadable(
            "NativeReadError: candidate path is not a regular file".into(),
        ));
    }
    ensure!(
        before.len() <= MAX_FILE as u64,
        "scan single read exceeds 1 MiB"
    );
    reads.reserve(before.len())?;
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(
        libc::O_NONBLOCK | libc::O_CLOEXEC | if secret { libc::O_NOFOLLOW } else { 0 },
    );
    let mut file = match unreadable(directory.open_with(path, &options)) {
        Ok(file) => file,
        Err(state) => return Ok(state),
    };
    let opened = match unreadable(file.metadata()) {
        Ok(meta) => meta,
        Err(state) => return Ok(state),
    };
    ensure!(
        same(&before, &opened),
        "scan candidate file changed before read"
    );
    let raw = match read_bytes(&mut file, usize::try_from(before.len())?, cancel) {
        Ok(raw) => raw,
        Err(error) => match error.downcast_ref::<io::Error>() {
            Some(error) => return Ok(Text::Unreadable(io_reason(error))),
            None => return Err(error),
        },
    };
    let after = match unreadable(file.metadata()) {
        Ok(meta) => meta,
        Err(state) => return Ok(state),
    };
    let current = if secret {
        directory.symlink_metadata(path)
    } else {
        directory.metadata(path)
    };
    let current = match unreadable(current) {
        Ok(meta) => meta,
        Err(state) => return Ok(state),
    };
    ensure!(
        same(&before, &after) && same(&before, &current),
        "scan candidate file or path changed during read"
    );
    live(cancel)?;
    match String::from_utf8(raw) {
        Ok(text) => Ok(Text::Included(normalize(text))),
        Err(_) if secret => Ok(Text::Skipped),
        Err(error) => Ok(Text::Unreadable(format!(
            "NativeUtf8Error: invalid UTF8 at byte {}",
            error.utf8_error().valid_up_to()
        ))),
    }
}
pub(super) fn prepare(
    selection: &Selection,
    directory: &Dir,
    id: &str,
    cancel: Option<&Cancellation>,
) -> Result<(Prepared, Option<Ledger>)> {
    let secret = id == "secret-scan";
    ensure!(secret || id == "native-source", "unsupported scan builtin");
    ensure!(
        selection.candidate.changes.len() <= MAX_FILES,
        "scan file count exceeds 100000"
    );
    let policy_limit = selection.policy.document["max_file_bytes"]
        .as_u64()
        .context("missing scan policy max_file_bytes")?;
    let mut reads = Reads { reserved: 0 };
    let mut budget = Budget::default();
    let mut ledger = Ledger::default();
    let mut files = Vec::new();
    let mut records = Vec::new();
    for change in &selection.candidate.changes {
        live(cancel)?;
        if change.deleted()
            || (secret && change.new_mode == "160000")
            || (!secret && !change.classes.iter().any(|class| class == "native"))
        {
            continue;
        }
        ensure!(change.path.len() <= 4096, "scan path exceeds 4096 bytes");
        budget.add(change.path.len() + 64)?;
        ledger.expected += 1;
        if !secret {
            files.push(change.path.clone());
        }
        match read_text(
            directory,
            &change.path,
            secret,
            policy_limit,
            &mut reads,
            cancel,
        )? {
            Text::Included(text) => {
                let record = json!({"path":change.path,"text":text});
                budget.add(crate::json_canon::encoded_size(&record, MAX_INPUT_BYTES)?)?;
                if secret {
                    files.push(change.path.clone());
                }
                records.push(record);
                ledger.read += 1;
            }
            Text::Skipped => {}
            Text::Unreadable(reason) if secret => {
                anyhow::bail!(
                    "secret scan cannot read candidate file {}: {reason}",
                    change.path
                )
            }
            Text::Unreadable(reason) => {
                budget.add(reason.len())?;
                ledger.skipped.insert(change.path.clone(), reason);
            }
        }
    }
    files.sort_unstable();
    files.dedup();
    Ok((
        Prepared {
            payload: json!({"files":records}),
            files,
        },
        if secret { None } else { Some(ledger) },
    ))
}
fn incomplete(path: String, reason: String) -> Finding {
    Finding {check_id:"native-source".into(),rule_id:"unreadable-changed-file".into(),severity:Severity::Critical,
        message:format!("changed native source could not be read ({reason}); this file is in the candidate change set, so the check cannot adjudicate the code under review"),
        path:Some(path),line:None,column:None,
        help:Some("Restore the file, or exclude it from the scan deliberately rather than by failing to open it.".into()),
        evidence:serde_json::Map::from_iter([("reason".into(),json!(reason)),("in_change_set".into(),json!(true))]),
        fingerprint:String::new(),exception_id:None,inherited:false}
}
pub(super) fn complete(value: &mut Value, ledger: Option<Ledger>) -> Result<()> {
    let Some(ledger) = ledger else {
        return Ok(());
    };
    let mut budget = Budget::default();
    budget.add(crate::json_canon::encoded_size(value, MAX_RESULT_BYTES)?)?;
    let metrics = json!({"files_expected":ledger.expected,"files_read":ledger.read,"files_skipped":ledger.skipped.len(),"skipped_files":ledger.skipped});
    budget.add(crate::json_canon::encoded_size(&metrics, MAX_RESULT_BYTES)?)?;
    let findings = value["findings"]
        .as_array_mut()
        .context("scan evaluator omitted findings array")?;
    for (path, reason) in ledger.skipped {
        ensure!(
            findings.len() < MAX_FILES,
            "native scan findings exceed 100000"
        );
        let finding = serde_json::to_value(incomplete(path, reason))?;
        budget.add(crate::json_canon::encoded_size(&finding, MAX_RESULT_BYTES)?)?;
        findings.push(finding);
    }
    value["metrics"] = metrics;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir(&self.0).expect("remove private scan fixture");
        }
    }
    #[test]
    fn absent_materialized_secret_path_is_unreadable_not_a_clean_skip() {
        use std::os::unix::fs::DirBuilderExt;
        let path = std::env::temp_dir().join(crate::resource_id("forge-scan-missing").unwrap());
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        let scratch = Scratch(path);
        let directory = Dir::open_ambient_dir(&scratch.0, cap_std::ambient_authority()).unwrap();
        let mut reads = Reads { reserved: 0 };
        let state = read_text(
            &directory,
            "missing-materialized.txt",
            true,
            1024,
            &mut reads,
            None,
        )
        .unwrap();
        let Text::Unreadable(reason) = state else {
            panic!("an unexpectedly missing materialized path must not be skipped");
        };
        assert!(reason.starts_with("NativeReadError: kind=NotFound, errno="));
        assert!(reason.len() < 128);
        assert!(!reason.contains("missing-materialized.txt"));
    }

    fn reader_directory() -> (Scratch, Dir) {
        use std::os::unix::fs::DirBuilderExt;
        let path = std::env::temp_dir().join(crate::resource_id("forge-scan-reader").unwrap());
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        let scratch = Scratch(path);
        let directory = Dir::open_ambient_dir(&scratch.0, cap_std::ambient_authority()).unwrap();
        (scratch, directory)
    }

    #[test]
    fn pre_cancelled_scan_refuses_before_missing_or_invalid_path_metadata() {
        let (_scratch, directory) = reader_directory();
        let cancel = Cancellation::new().unwrap();
        cancel.cancel().unwrap();
        for secret in [false, true] {
            for path in ["absent-before-cancellation.txt", "\0invalid-path"] {
                let mut reads = Reads { reserved: 0 };
                let attempted =
                    read_text(&directory, path, secret, 1024, &mut reads, Some(&cancel));
                let Err(error) = attempted else {
                    panic!("pre-cancelled scan must refuse before metadata or skipping");
                };
                assert!(error.to_string().contains("builtin check cancelled"));
            }
        }
    }

    fn unlinked_file(directory: &Dir, bytes: &[u8]) -> cap_std::fs::File {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = directory
            .open_with(
                "sample",
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .mode(0o600),
            )
            .unwrap();
        file.write_all(bytes).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        // Reader owns the descriptor; fixture cleanup remains empty even if an
        // assertion fails. No concurrent replacement, truncation or hooks.
        directory.remove_file("sample").unwrap();
        file
    }

    #[test]
    fn descriptor_bytes_must_match_advertised_size_without_timing_races() {
        let (_scratch, directory) = reader_directory();
        for (bytes, advertised, reason) in [
            (&b"abc"[..], 4, "shrank"),
            (&b"abcd"[..], 3, "grew"),
            (&b"x"[..], 0, "grew"),
        ] {
            let mut file = unlinked_file(&directory, bytes);
            let error = read_bytes(&mut file, advertised, None).unwrap_err();
            assert!(
                error.to_string().contains(reason),
                "wrong size-mismatch diagnostic"
            );
        }
        let mut exact = unlinked_file(&directory, b"abc");
        assert_eq!(read_bytes(&mut exact, 3, None).unwrap().as_slice(), b"abc");
    }

    #[test]
    fn rejected_utf8_read_keeps_credit_and_blocks_following_small_read() {
        let (_scratch, directory) = reader_directory();
        for secret in [false, true] {
            // Only three bytes of credit remain. One invalid byte plus its EOF
            // allowance consumes two; the next one-byte read must then refuse.
            let mut reads = Reads {
                reserved: MAX_INPUT_BYTES - 3,
            };
            directory.write("invalid", b"\xff").unwrap();
            let rejected = read_text(&directory, "invalid", secret, 1024, &mut reads, None);
            directory.remove_file("invalid").unwrap();
            match (secret, rejected.unwrap()) {
                (true, Text::Skipped) => {}
                (false, Text::Unreadable(reason)) => {
                    assert!(reason.starts_with("NativeUtf8Error:"))
                }
                _ => panic!("invalid UTF8 must not produce included scan text"),
            }
            directory.write("next", b"x").unwrap();
            let following = read_text(&directory, "next", secret, 1024, &mut reads, None);
            directory.remove_file("next").unwrap();
            let Err(error) = following else {
                panic!("failed UTF8 attempt must not refund aggregate read credit");
            };
            assert!(error.to_string().contains("attempted reads exceed 32 MiB"));
        }
    }
}
