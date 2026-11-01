//! Transactional, evidence-bound replacement of individual receipt cells.
use super::{cell, dispatch};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(super) struct Transaction {
    output: PathBuf,
    bytes: Vec<u8>,
    pub(super) receipt: Value,
    _lock: forge::file_lock::Exclusive,
}
impl Transaction {
    pub(super) fn open(output: &Path) -> Result<Self> {
        let output = output.canonicalize()?;
        let lock = lock(&output)?;
        let bytes = forge::bounded_file::read(&output.join("receipt.json"), 1024 * 1024)?;
        let receipt = forge::tool_schema::parse_document(std::str::from_utf8(&bytes)?)?;
        validate(&receipt)?;
        Ok(Self {
            output,
            bytes,
            receipt,
            _lock: lock,
        })
    }
    pub(super) fn replace(self, replacements: Vec<Value>) -> Result<Value> {
        let mut map = Map::new();
        for replacement in replacements {
            let id = replacement["cell_id"]
                .as_str()
                .context("replacement cell id")?
                .to_owned();
            ensure!(
                self.receipt["cells"]
                    .as_array()
                    .context("receipt cells missing")?
                    .iter()
                    .any(|row| row["cell_id"] == id),
                "replacement cell is absent: {id}"
            );
            ensure!(
                map.insert(id, replacement).is_none(),
                "duplicate replacement cell"
            );
        }
        let mut updated = dispatch(
            "replace_receipt_cells",
            &json!({"receipt":self.receipt,"replacements":map}),
        )?;
        validate(&updated)?;
        let archive = self
            .output
            .join(forge::resource_id("receipt-pre-reconcile")? + ".json");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&archive)?;
        file.write_all(&self.bytes)?;
        file.sync_all()?;
        fs::File::open(&self.output)?.sync_all()?;
        updated["provenance"]["reconciled_at"] =
            json!(forge::instant::isoformat_utc(forge::instant::now()));
        updated["provenance"]["pre_reconcile_receipt"] = json!(archive);
        updated["provenance"]["pre_reconcile_receipt_sha256"] =
            json!(format!("{:x}", Sha256::digest(&self.bytes)));
        let bytes = serde_json::to_vec_pretty(&updated)?;
        ensure!(
            bytes.len() <= 1024 * 1024,
            "reconciled receipt exceeds 1 MiB"
        );
        forge::state_publish::publish(&self.output.join("receipt.json"), &bytes)?;
        Ok(updated)
    }
}
pub(super) fn lock(output: &Path) -> Result<forge::file_lock::Exclusive> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(output.join(".runtime-reconcile.lock"))?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o077 == 0,
        "runtime receipt lock must be private, owned and singly linked"
    );
    forge::file_lock::Exclusive::try_new(file, "runtime receipt writer")
}
pub(super) fn validate(receipt: &Value) -> Result<()> {
    ensure!(
        receipt["schema_version"] == 1 && receipt["provenance"].is_object(),
        "invalid runtime receipt schema"
    );
    let cells = receipt["cells"]
        .as_array()
        .context("runtime receipt cells missing")?;
    ensure!(
        cells.len() == 9,
        "runtime receipt must contain exactly nine cells"
    );
    let expected: BTreeSet<_> = [
        "active-state-live-claims",
        "hook-config-contract",
        "hook-program-controls",
        "launcher-programs",
        "embedding-canary",
        "retriever-runtime",
        "graph-semantic-runtime",
        "launcher-real-smokes",
        "local-clerk-canary",
    ]
    .into_iter()
    .collect();
    let mut found = BTreeSet::new();
    for row in cells {
        let id = row["cell_id"].as_str().context("receipt cell id missing")?;
        ensure!(
            expected.contains(id) && found.insert(id),
            "unknown or duplicate receipt cell {id:?}"
        );
        ensure!(
            row["required"] == true
                && ["PASS", "NOT_READY", "FAIL-CLOSED"]
                    .contains(&row["status"].as_str().unwrap_or("")),
            "invalid receipt cell {id:?}"
        );
        ensure!(
            row["detail"].is_string() && row["evidence"].is_object(),
            "invalid receipt cell evidence {id:?}"
        );
    }
    let status = dispatch("aggregate_status", &json!(cells))?;
    ensure!(
        receipt["status"] == status,
        "runtime receipt status disagrees with cells"
    );
    Ok(())
}

pub(super) fn launcher_cell(output: &Path, receipt: &Value) -> Result<Value> {
    let root = output.canonicalize()?;
    let cells = receipt["cells"]
        .as_array()
        .context("receipt cells missing")?;
    let previous = cells
        .iter()
        .find(|row| row["cell_id"] == "launcher-real-smokes")
        .context("launcher cell missing")?;
    let rows = previous["evidence"]["launchers"]
        .as_object()
        .context("launcher observations missing")?;
    let mut updated = Map::new();
    let mut failed = false;
    let mut unavailable = false;
    let mut total = 0_i64;
    for name in ["codex", "claude", "glm", "qwen", "grok"] {
        let Some(mut row) = rows.get(name).cloned() else {
            unavailable = true;
            continue;
        };
        let status = row["status"]
            .as_str()
            .context("launcher status missing")?
            .to_owned();
        ensure!(
            ["PASS", "NOT_READY", "FAIL-CLOSED"].contains(&status.as_str()),
            "invalid launcher status"
        );
        failed |= status == "FAIL-CLOSED";
        unavailable |= status == "NOT_READY";
        if status == "PASS" {
            ensure!(
                row["safe_marker_seen"] == true
                    && row["deny_marker_seen"] == true
                    && row["final_ack_seen"] == true
                    && row["secret_marker_seen"] == false
                    && row["returncode"] == 0,
                "passing launcher evidence is incomplete: {name}"
            );
        }
        if let Some(path) = row["log"].as_str() {
            let path = Path::new(path);
            ensure!(
                fs::symlink_metadata(path)?.file_type().is_file()
                    && path.canonicalize()?.starts_with(&root),
                "launcher log escapes output or is not a regular file"
            );
            let bytes = forge::bounded_file::read(path, 1024 * 1024)?;
            let digest = format!("{:x}", Sha256::digest(&bytes));
            ensure!(
                row["log_sha256"].as_str() == Some(digest.as_str()),
                "launcher log hash mismatch: {name}"
            );
            let tokens = dispatch(
                "extract_reported_tokens",
                &json!(std::str::from_utf8(&bytes)?),
            )?
            .as_i64()
            .context("launcher token count missing")?;
            ensure!(tokens >= 0, "negative launcher token count");
            total = total.saturating_add(tokens);
            row["reported_tokens"] = json!(tokens);
            if status == "PASS" && tokens == 0 {
                failed = true;
            }
        } else {
            ensure!(status == "NOT_READY", "launcher log missing: {name}");
        }
        updated.insert(name.to_owned(), row);
    }
    failed |= total > 250_000;
    Ok(cell(
        "launcher-real-smokes",
        if failed {
            "FAIL-CLOSED"
        } else if unavailable {
            "NOT_READY"
        } else {
            "PASS"
        },
        "reconciled token counts from hash-bound launcher logs",
        json!({"launchers":updated,"reported_tokens":total,
        "usage_accounting":"maximum cumulative usage; cached breakdown excluded"}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launcher_reconciliation_requires_hash_bound_logs() {
        let root = std::env::temp_dir().join(forge::resource_id("reconcile-test").unwrap());
        fs::create_dir(&root).unwrap();
        let mut rows = Map::new();
        for name in ["codex", "claude", "glm", "qwen", "grok"] {
            let path = root.join(format!("{name}.log"));
            let bytes = b"{\"usage\":{\"total_tokens\":2}}\n";
            fs::write(&path, bytes).unwrap();
            rows.insert(name.into(), json!({"status":"PASS","log":path,"log_sha256":format!("{:x}",Sha256::digest(bytes)),
                "safe_marker_seen":true,"deny_marker_seen":true,"final_ack_seen":true,"secret_marker_seen":false,"returncode":0}));
        }
        let receipt =
            json!({"cells":[{"cell_id":"launcher-real-smokes","evidence":{"launchers":rows}}]});
        let cell = launcher_cell(&root, &receipt).unwrap();
        assert_eq!(cell["status"], "PASS");
        assert_eq!(cell["evidence"]["reported_tokens"], 10);
        fs::write(root.join("codex.log"), b"tampered").unwrap();
        assert!(launcher_cell(&root, &receipt).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
