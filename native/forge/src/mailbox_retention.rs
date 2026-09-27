//! Bounded, manual A2A retention over existing SQLite stores.
//! Preview is read-only; apply requires one exact store and matching actor.

use anyhow::{ensure, Context, Result};
use clap::Args;
use conductor_native::a2a_retention::{self as core, RetentionRow};
use rusqlite::{params, Connection, OpenFlags, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use walkdir::WalkDir;

const DEFAULT_BATCH: usize = 100;
const MAX_BATCH: usize = 1_000;
const MAX_EVIDENCE_FILES: usize = 512;
const MAX_EVIDENCE_FILE_BYTES: usize = 2 << 20;
const MAX_EVIDENCE_TOTAL_BYTES: usize = 32 << 20;

#[derive(Args)]
pub struct RetentionArgs {
    /// Host evidence root, ordinarily the project checkout.
    #[arg(long)]
    evidence_root: Option<PathBuf>,
    /// Exact store name; repeatable for preview, exactly one for apply.
    #[arg(long = "store")]
    stores: Vec<String>,
    /// Acting agent; apply requires this to equal the sole store name.
    #[arg(long = "as-name")]
    actor: Option<String>,
    #[arg(long, default_value_t = 48.0)]
    grace_hours: f64,
    #[arg(long, default_value_t = DEFAULT_BATCH)]
    limit: usize,
    /// Override the evaluation time with an aware ISO-8601 timestamp.
    #[arg(long)]
    now: Option<String>,
    /// Atomically tombstone eligible content after checking evidence twice.
    #[arg(long)]
    apply: bool,
}

struct Candidate {
    manifest: RetentionRow,
    delivery_status: String,
    status_reason: Option<String>,
    hold_reason: Option<String>,
    tombstoned_at: Option<String>,
}

#[derive(PartialEq, Eq)]
struct Evidence {
    paths: Vec<String>,
    protected_ids: Vec<String>,
    sha256: String,
}

struct Batch {
    candidates: Vec<Candidate>,
    ids: Vec<String>,
    evidence: Evidence,
    manifests: core::ManifestResult,
}

fn grace_seconds(hours: f64) -> Result<f64> {
    let seconds = hours * 3600.0;
    ensure!(
        seconds.is_finite() && seconds >= 3600.0,
        "retention grace must be finite and at least 1 hour"
    );
    Ok(seconds)
}

fn store_names(
    state_dir: &Path,
    requested: &[String],
    apply: bool,
    actor: Option<&str>,
) -> Result<Vec<String>> {
    if apply {
        ensure!(
            requested.len() == 1,
            "--apply requires exactly one explicit --store"
        );
        ensure!(
            actor == Some(requested[0].as_str()),
            "--apply requires --as-name matching the exact --store"
        );
    }
    let names = if requested.is_empty() {
        let mut found = Vec::new();
        if state_dir.is_dir() {
            for entry in fs::read_dir(state_dir)? {
                let entry = entry?;
                if entry.path().join("store.sqlite").is_file() {
                    found.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        found.sort();
        found
    } else {
        requested.to_vec()
    };
    ensure!(
        names.iter().collect::<HashSet<_>>().len() == names.len(),
        "duplicate --store values are not allowed"
    );
    for name in &names {
        super::store::validate_identity(name)?;
    }
    Ok(names)
}

fn store_path(state_dir: &Path, name: &str) -> Result<PathBuf> {
    let path = state_dir.join(name).join("store.sqlite");
    ensure!(
        path.is_file(),
        "requested A2A store does not exist: {name:?}"
    );
    let root = state_dir
        .canonicalize()
        .with_context(|| format!("resolving state directory {}", state_dir.display()))?;
    ensure!(
        path.canonicalize()?.starts_with(root),
        "requested A2A store escapes the state directory: {name:?}"
    );
    Ok(path)
}

fn validate_schema(conn: &Connection) -> Result<()> {
    const TABLES: &[(&str, &[&str])] = &[
        (
            "messages",
            &[
                "direction",
                "message_id",
                "sender",
                "recipient",
                "body",
                "data_json",
                "created_at",
                "received_at",
                "delivery_status",
                "status_reason",
                "read_at",
            ],
        ),
        (
            "message_state",
            &[
                "direction",
                "message_id",
                "thread_id",
                "summary",
                "protocol_status",
                "requires_response",
                "retention_class",
                "resolved_at",
                "superseded_at",
                "hold_reason",
                "tombstoned_at",
                "body_sha256",
                "body_bytes",
                "data_sha256",
                "data_bytes",
            ],
        ),
        (
            "retention_events",
            &[
                "event_id",
                "direction",
                "message_id",
                "policy_version",
                "manifest_json",
                "manifest_sha256",
                "compacted_at",
            ],
        ),
    ];
    for &(table, expected) in TABLES {
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
        let found = stmt
            .query_map([table], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        let missing = expected
            .iter()
            .filter(|name| !found.contains(**name))
            .copied()
            .collect::<Vec<_>>();
        ensure!(
            missing.is_empty(),
            "A2A store schema is missing {table} columns: {missing:?}"
        );
    }
    Ok(())
}

fn candidates(tx: &Transaction<'_>, cutoff: &str) -> Result<Vec<Candidate>> {
    let mut stmt = tx.prepare("SELECT m.message_id,m.direction,m.sender,m.recipient,m.body,m.data_json,m.created_at,m.received_at,m.delivery_status,m.status_reason,m.read_at,s.thread_id,s.summary,s.protocol_status,s.requires_response,s.retention_class,s.resolved_at,s.superseded_at,s.hold_reason,s.tombstoned_at,s.body_sha256,s.body_bytes,s.data_sha256,s.data_bytes
        FROM messages m JOIN message_state s ON s.direction=m.direction AND s.message_id=m.message_id
        WHERE m.direction='inbound' AND m.read_at IS NOT NULL AND s.retention_class='operational'
          AND s.protocol_status IN ('resolved','superseded') AND s.requires_response=0
          AND s.hold_reason IS NULL AND s.tombstoned_at IS NULL
          AND (s.resolved_at IS NOT NULL OR s.superseded_at IS NOT NULL)
          AND (s.resolved_at IS NULL OR s.resolved_at <= ?1)
          AND (s.superseded_at IS NULL OR s.superseded_at <= ?1)
          AND NOT EXISTS (SELECT 1 FROM retention_events e WHERE e.direction=m.direction AND e.message_id=m.message_id)
        ORDER BY CASE WHEN s.resolved_at IS NULL THEN s.superseded_at
                      WHEN s.superseded_at IS NULL THEN s.resolved_at
                      WHEN s.resolved_at >= s.superseded_at THEN s.resolved_at ELSE s.superseded_at END,
                 m.message_id LIMIT ?2")?;
    let rows = stmt.query_map(params![cutoff, MAX_BATCH as i64], |row| {
        let response: i64 = row.get(14)?;
        let manifest = RetentionRow {
            message_id: row.get(0)?,
            direction: row.get(1)?,
            sender: row.get(2)?,
            recipient: row.get(3)?,
            body: row.get(4)?,
            data_json: row.get(5)?,
            created_at: row.get(6)?,
            received_at: row.get(7)?,
            read_at: row.get(10)?,
            thread_id: row.get(11)?,
            summary: row.get(12)?,
            protocol_status: row.get(13)?,
            requires_response: response != 0,
            retention_class: row.get(15)?,
            resolved_at: row.get(16)?,
            superseded_at: row.get(17)?,
            body_sha256: row.get(20)?,
            body_bytes: row.get(21)?,
            data_sha256: row.get(22)?,
            data_bytes: row.get(23)?,
        };
        Ok(Candidate {
            manifest,
            delivery_status: row.get(8)?,
            status_reason: row.get(9)?,
            hold_reason: row.get(18)?,
            tombstoned_at: row.get(19)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn evidence_paths(root: &Path) -> Result<Vec<PathBuf>> {
    ensure!(
        root.is_dir(),
        "evidence root is not a directory: {}",
        root.display()
    );
    let reports = root.join("research/reports");
    if !reports.exists() {
        return Ok(Vec::new());
    }
    let mut matches = Vec::new();
    for entry in WalkDir::new(&reports).follow_links(false) {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy();
        if name.ends_with(".json") && (name.contains("gate") || name.contains("receipt")) {
            matches.push(entry.path().to_path_buf());
        }
    }
    matches.sort();
    ensure!(
        matches.len() <= MAX_EVIDENCE_FILES,
        "evidence scan found {} files; maximum is {MAX_EVIDENCE_FILES}",
        matches.len()
    );
    Ok(matches)
}

fn evidence_snapshot(root: &Path, ids: &[String]) -> Result<Evidence> {
    let resolved_root = root.canonicalize()?;
    let mut files = Vec::new();
    let mut total = 0usize;
    for path in evidence_paths(root)? {
        let resolved = path
            .canonicalize()
            .with_context(|| format!("cannot safely read evidence file {}", path.display()))?;
        let relative = resolved
            .strip_prefix(&resolved_root)
            .with_context(|| format!("evidence path escapes root: {}", path.display()))?;
        ensure!(
            resolved.is_file(),
            "evidence path is not a regular file: {}",
            path.display()
        );
        let size = resolved.metadata()?.len();
        ensure!(
            size <= MAX_EVIDENCE_FILE_BYTES as u64,
            "evidence file exceeds {MAX_EVIDENCE_FILE_BYTES} bytes: {}",
            path.display()
        );
        let mut raw = Vec::with_capacity(size as usize);
        File::open(&resolved)?
            .take((MAX_EVIDENCE_FILE_BYTES + 1) as u64)
            .read_to_end(&mut raw)?;
        ensure!(
            raw.len() as u64 == size,
            "evidence file changed while being read: {}",
            path.display()
        );
        total += raw.len();
        ensure!(
            total <= MAX_EVIDENCE_TOTAL_BYTES,
            "evidence scan exceeds {MAX_EVIDENCE_TOTAL_BYTES} total bytes"
        );
        files.push((relative.to_string_lossy().replace('\\', "/"), raw));
    }
    let (paths, protected_ids, sha256) = core::evidence(ids, &files).map_err(anyhow::Error::msg)?;
    Ok(Evidence {
        paths,
        protected_ids,
        sha256,
    })
}

fn validated_time(value: &str, field: &str) -> Result<f64> {
    // instant::parse intentionally accepts a narrow ISO subset, but its
    // calendar arithmetic normalizes dates such as February 31. Retention
    // must reject those exactly as datetime.fromisoformat does.
    let bytes = value.as_bytes();
    ensure!(
        bytes.is_ascii() && bytes.len() >= 10,
        "{field} is not a valid timezone-aware ISO-8601 timestamp: {value:?}"
    );
    let year = value[0..4].parse::<i64>().ok();
    let month = value[5..7].parse::<u32>().ok();
    let day = value[8..10].parse::<u32>().ok();
    if let (Some(year), Some(month), Some(day)) = (year, month, day) {
        ensure!(
            year >= 1
                && (1..=12).contains(&month)
                && (1..=31).contains(&day)
                && crate::civil::civil_from_days(crate::civil::days_from_civil(year, month, day))
                    == (year, month, day),
            "{field} is not a valid timezone-aware ISO-8601 timestamp: {value:?}"
        );
    }
    crate::instant::parse(value).with_context(|| {
        format!("{field} is not a valid timezone-aware ISO-8601 timestamp: {value:?}")
    })
}

fn validate_candidate_time(row: &Candidate, now: f64, grace: f64) -> Result<()> {
    let terminal = [
        row.manifest.resolved_at.as_deref(),
        row.manifest.superseded_at.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(|value| validated_time(value, "lifecycle timestamp"))
    .collect::<Result<Vec<_>>>()?;
    let latest = terminal
        .into_iter()
        .reduce(f64::max)
        .context("retention candidate has no terminal lifecycle timestamp")?;
    let read = validated_time(&row.manifest.read_at, "read_at")?;
    ensure!(
        latest <= now && read <= now,
        "future A2A timestamps are not retention-eligible"
    );
    ensure!(
        latest <= now - grace,
        "retention candidate is newer than the grace cutoff"
    );
    Ok(())
}

fn prepare(
    tx: &Transaction<'_>,
    evidence_root: &Path,
    cutoff: &str,
    limit: usize,
    now: f64,
    grace: f64,
    stamped: &str,
) -> Result<Batch> {
    let all = candidates(tx, cutoff)?;
    let ids = all
        .iter()
        .map(|row| row.manifest.message_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let evidence = evidence_snapshot(evidence_root, &ids)?;
    let protected = evidence.protected_ids.iter().collect::<HashSet<_>>();
    let selected = all
        .into_iter()
        .filter(|row| !protected.contains(&row.manifest.message_id))
        .take(limit)
        .collect::<Vec<_>>();
    for row in &selected {
        validate_candidate_time(row, now, grace)?;
    }
    let manifests = core::manifests(
        &selected
            .iter()
            .map(|row| row.manifest.clone())
            .collect::<Vec<_>>(),
        stamped,
    )
    .map_err(anyhow::Error::msg)?;
    Ok(Batch {
        candidates: selected,
        ids,
        evidence,
        manifests,
    })
}

fn apply_row(
    tx: &Transaction<'_>,
    row: &Candidate,
    item: &core::ManifestItem,
    stamped: &str,
) -> Result<()> {
    let state = &row.manifest;
    tx.execute("INSERT INTO retention_events(event_id,direction,message_id,policy_version,manifest_json,manifest_sha256,compacted_at)
                VALUES(?1,'inbound',?2,?3,?4,?5,?6)",
        params![item.3, state.message_id, core::POLICY_VERSION as i64, item.1, item.2, stamped])?;
    let changed = tx.execute(
        "UPDATE messages SET body=?1,data_json=NULL
        WHERE direction='inbound' AND message_id=?2 AND body=?3
          AND ((data_json IS NULL AND ?4 IS NULL) OR data_json=?4)
          AND sender=?5 AND recipient=?6 AND created_at=?7 AND received_at IS ?8
          AND delivery_status=?9 AND status_reason IS ?10 AND read_at=?11",
        params![
            core::TOMBSTONE_BODY,
            state.message_id,
            state.body,
            state.data_json,
            state.sender,
            state.recipient,
            state.created_at,
            state.received_at,
            row.delivery_status,
            row.status_reason,
            state.read_at
        ],
    )?;
    ensure!(
        changed == 1,
        "message {:?} changed during retention",
        state.message_id
    );
    let changed = tx.execute(
        "UPDATE message_state SET tombstoned_at=?1
        WHERE direction='inbound' AND message_id=?2 AND thread_id=?3 AND summary=?4
          AND protocol_status=?5 AND requires_response=?6 AND retention_class=?7
          AND hold_reason IS ?8 AND tombstoned_at IS ?9 AND body_sha256=?10
          AND body_bytes=?11 AND data_sha256 IS ?12 AND data_bytes=?13
          AND resolved_at IS ?14 AND superseded_at IS ?15",
        params![
            stamped,
            state.message_id,
            state.thread_id,
            state.summary,
            state.protocol_status,
            i64::from(state.requires_response),
            state.retention_class,
            row.hold_reason,
            row.tombstoned_at,
            state.body_sha256,
            state.body_bytes,
            state.data_sha256,
            state.data_bytes,
            state.resolved_at,
            state.superseded_at
        ],
    )?;
    ensure!(
        changed == 1,
        "message {:?} lifecycle changed during retention",
        state.message_id
    );
    Ok(())
}

fn result(path: &Path, apply: bool, batch: &Batch) -> Value {
    json!({
        "store": path.to_string_lossy(), "mode": if apply {"apply"} else {"preview"},
        "eligible": batch.candidates.len(), "compacted": if apply {batch.candidates.len()} else {0},
        "original_content_bytes": batch.manifests.1, "tombstone_bytes": batch.manifests.2,
        "logical_bytes_removed": batch.manifests.3, "evidence_files": batch.evidence.paths.len(),
        "evidence_protected": batch.evidence.protected_ids.len(),
        "evidence_snapshot_sha256": batch.evidence.sha256,
        "manifest_sha256": batch.manifests.0.iter().map(|item| &item.2).collect::<Vec<_>>(),
        "event_sha256": batch.manifests.0.iter().map(|item| &item.3).collect::<Vec<_>>(),
    })
}

fn compact(
    path: &Path,
    evidence_root: &Path,
    now: f64,
    grace: f64,
    limit: usize,
    apply: bool,
) -> Result<Value> {
    compact_with_recheck(path, evidence_root, now, grace, limit, apply, || Ok(()))
}

fn compact_with_recheck(
    path: &Path,
    evidence_root: &Path,
    now: f64,
    grace: f64,
    limit: usize,
    apply: bool,
    before_recheck: impl FnOnce() -> Result<()>,
) -> Result<Value> {
    let flags = if apply {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let mut conn = Connection::open_with_flags(path, flags)
        .with_context(|| format!("opening A2A store {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    validate_schema(&conn)?;
    let stamped = crate::instant::isoformat_millis_utc(now);
    let cutoff = crate::instant::isoformat_millis_utc(now - grace);
    let tx = conn.transaction_with_behavior(if apply {
        TransactionBehavior::Immediate
    } else {
        TransactionBehavior::Deferred
    })?;
    let batch = prepare(&tx, evidence_root, &cutoff, limit, now, grace, &stamped)?;
    if apply {
        for (row, item) in batch.candidates.iter().zip(&batch.manifests.0) {
            apply_row(&tx, row, item, &stamped)?;
        }
        before_recheck()?;
        ensure!(
            evidence_snapshot(evidence_root, &batch.ids)? == batch.evidence,
            "retention evidence changed during compaction"
        );
        tx.commit()?;
    }
    Ok(result(path, apply, &batch))
}

pub fn run(args: RetentionArgs, state_dir: &Path, host: &Path) -> Result<Value> {
    let grace = grace_seconds(args.grace_hours)?;
    ensure!(
        (1..=MAX_BATCH).contains(&args.limit),
        "retention limit must be an integer in 1..{MAX_BATCH}"
    );
    let now = match args.now {
        Some(raw) => validated_time(&raw, "now")?,
        None => crate::instant::now(),
    };
    ensure!(
        now.is_finite() && (now - grace).is_finite(),
        "retention time is out of range"
    );
    let names = store_names(state_dir, &args.stores, args.apply, args.actor.as_deref())?;
    let evidence_root = args.evidence_root.as_deref().unwrap_or(host);
    let mut results = Vec::with_capacity(names.len());
    for name in names {
        let path = store_path(state_dir, &name)?;
        results.push(compact(
            &path,
            evidence_root,
            now,
            grace,
            args.limit,
            args.apply,
        )?);
    }
    Ok(
        json!({"schema_version":1,"authority":"deterministic-a2a-retention",
        "automatic":false,"mode":if args.apply {"apply"} else {"preview"},"results":results}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn evidence_appearing_between_snapshot_and_commit_rolls_back_every_write() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "forge-retention-drift-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("store.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE messages(direction TEXT,message_id TEXT,sender TEXT,recipient TEXT,body TEXT,data_json TEXT,created_at TEXT,received_at TEXT,delivery_status TEXT,status_reason TEXT,read_at TEXT,PRIMARY KEY(direction,message_id));
            CREATE TABLE message_state(direction TEXT,message_id TEXT,thread_id TEXT,summary TEXT,protocol_status TEXT,requires_response INTEGER,retention_class TEXT,resolved_at TEXT,superseded_at TEXT,hold_reason TEXT,tombstoned_at TEXT,body_sha256 TEXT,body_bytes INTEGER,data_sha256 TEXT,data_bytes INTEGER,PRIMARY KEY(direction,message_id));
            CREATE TABLE retention_events(event_id TEXT PRIMARY KEY,direction TEXT,message_id TEXT,policy_version INTEGER,manifest_json TEXT,manifest_sha256 TEXT,compacted_at TEXT,UNIQUE(direction,message_id));")
            .unwrap();
        let old = "2026-08-30T09:00:00.000+00:00";
        let body = "retained content";
        let digest = format!("{:x}", Sha256::digest(body.as_bytes()));
        conn.execute("INSERT INTO messages VALUES ('inbound','eligible','sender','worker',?1,NULL,?2,?2,'delivered',NULL,?2)",
            params![body, old]).unwrap();
        conn.execute("INSERT INTO message_state VALUES ('inbound','eligible','thread','summary','resolved',0,'operational',?1,NULL,NULL,NULL,?2,?3,NULL,0)",
            params![old, digest, body.len() as i64]).unwrap();
        let now = crate::instant::parse("2026-08-30T12:00:00.000+00:00").unwrap();
        let reports = root.join("research/reports");
        fs::create_dir_all(&reports).unwrap();
        let result = compact_with_recheck(&path, &root, now, 3600.0, 100, true, || {
            fs::write(
                reports.join("late_gate_receipt.json"),
                r#"{"message_id":"eligible"}"#,
            )?;
            Ok(())
        });
        assert!(result.unwrap_err().to_string().contains("evidence changed"));
        let stored_body: String = conn
            .query_row("SELECT body FROM messages", [], |row| row.get(0))
            .unwrap();
        let events: i64 = conn
            .query_row("SELECT count(*) FROM retention_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(stored_body, body);
        assert_eq!(events, 0);
        drop(conn);
        fs::remove_dir_all(root).unwrap();
    }
}
