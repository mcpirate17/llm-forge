//! Transactional local task journal. A task result is execution evidence, never
//! an approval or a replacement for the host's governance gate.

use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Assigned,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Receipt {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub state: State,
    pub owner: Option<String>,
    pub command: Vec<String>,
    pub depends_on: Vec<String>,
    pub claim_id: Option<String>,
    pub session_id: Option<String>,
    pub message_id: Option<String>,
    pub attempt: u32,
    pub max_attempts: u32,
    pub lease_until: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub exit_code: Option<i32>,
    pub detail: Option<String>,
    pub receipt: Option<Receipt>,
}

#[derive(Debug, Serialize)]
pub struct Event {
    pub sequence: i64,
    pub at: i64,
    pub actor: String,
    pub action: String,
    pub attempt: u32,
    pub detail: Option<String>,
}

pub struct Store {
    connection: Connection,
}

pub fn path(host: &Path) -> PathBuf {
    host.join(".agents/forge/tasks.sqlite3")
}

pub fn validate_id(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "task id must be 1..128 ASCII letters, digits, '-' or '_'"
    );
    Ok(())
}

pub fn validate_owner(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control),
        "owner must be nonempty, at most 256 bytes, without control characters"
    );
    Ok(())
}

fn load(connection: &Connection, id: &str) -> Result<Task> {
    let raw: Option<String> = connection
        .query_row("SELECT payload FROM tasks WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    serde_json::from_str(&raw.with_context(|| format!("unknown task {id}"))?)
        .with_context(|| format!("invalid task record {id}"))
}

impl Store {
    pub fn set_busy_timeout(&self, timeout: Duration) -> Result<()> {
        self.connection.busy_timeout(timeout)?;
        Ok(())
    }

    pub fn open(host: &Path, write: bool) -> Result<Option<Self>> {
        let path = path(host);
        if !write && !path.exists() {
            return Ok(None);
        }
        if write {
            std::fs::create_dir_all(path.parent().context("task database parent")?)?;
        }
        let flags = if write {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        };
        let connection = Connection::open_with_flags(&path, flags)
            .with_context(|| format!("opening task database {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(version <= 1, "unsupported task database schema {version}");
        if write {
            connection.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
                 CREATE TABLE IF NOT EXISTS tasks (
                   id TEXT PRIMARY KEY, state TEXT NOT NULL, owner TEXT,
                   updated_at INTEGER NOT NULL, payload TEXT NOT NULL CHECK(json_valid(payload)));
                 CREATE INDEX IF NOT EXISTS tasks_state ON tasks(state,updated_at);
                 CREATE TABLE IF NOT EXISTS events (
                   sequence INTEGER PRIMARY KEY, task_id TEXT NOT NULL, at INTEGER NOT NULL,
                   actor TEXT NOT NULL, action TEXT NOT NULL, attempt INTEGER NOT NULL, detail TEXT);
                 CREATE INDEX IF NOT EXISTS events_task ON events(task_id,sequence);
                 PRAGMA user_version=1;"
            )?;
        } else {
            ensure!(version == 1, "uninitialized task database schema {version}");
        }
        Ok(Some(Self { connection }))
    }

    pub fn create(&mut self, task: Task, actor: &str) -> Result<Task> {
        validate_id(&task.id)?;
        validate_owner(actor)?;
        if let Some(owner) = &task.owner {
            validate_owner(owner)?;
        }
        ensure!(
            !task.title.trim().is_empty(),
            "task title must not be empty"
        );
        ensure!(
            (1..=100).contains(&task.max_attempts),
            "max-attempts must be 1..100"
        );
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for dependency in &task.depends_on {
            load(&tx, dependency)?;
        }
        tx.execute(
            "INSERT INTO tasks VALUES (?1,?2,?3,?4,?5)",
            params![
                task.id,
                serde_json::to_string(&task.state)?,
                task.owner,
                task.updated_at,
                serde_json::to_string(&task)?
            ],
        )
        .with_context(|| format!("creating task {} (IDs cannot be reused)", task.id))?;
        record(&tx, &task, actor, "create")?;
        tx.commit()?;
        Ok(task)
    }

    pub fn get(&self, id: &str) -> Result<Task> {
        load(&self.connection, id)
    }

    pub fn list(&self, limit: usize) -> Result<Vec<Task>> {
        ensure!((1..=1000).contains(&limit), "limit must be 1..1000");
        let mut statement = self
            .connection
            .prepare("SELECT payload FROM tasks ORDER BY updated_at DESC,id LIMIT ?1")?;
        let rows = statement.query_map([limit], |r| r.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn counts(&self) -> Result<std::collections::BTreeMap<String, i64>> {
        let mut statement = self
            .connection
            .prepare("SELECT state,count(*) FROM tasks GROUP BY state")?;
        let rows = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?;
        rows.map(|row| {
            let (state, count) = row?;
            Ok((serde_json::from_str(&state)?, count))
        })
        .collect()
    }

    pub fn expired_count(&self, now: i64) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT count(*) FROM tasks WHERE state='\"running\"' AND json_extract(payload,'$.lease_until')<=?1",
            [now], |row| row.get(0))?)
    }

    pub fn events(&self, id: &str, limit: usize) -> Result<Vec<Event>> {
        self.get(id)?;
        ensure!((1..=1000).contains(&limit), "limit must be 1..1000");
        let mut statement = self.connection.prepare(
            "SELECT sequence,at,actor,action,attempt,detail FROM events
             WHERE task_id=?1 ORDER BY sequence DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![id, limit], |r| {
            Ok(Event {
                sequence: r.get(0)?,
                at: r.get(1)?,
                actor: r.get(2)?,
                action: r.get(3)?,
                attempt: r.get(4)?,
                detail: r.get(5)?,
            })
        })?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    pub fn update<F>(&mut self, id: &str, actor: &str, action: &str, now: i64, f: F) -> Result<Task>
    where
        F: FnOnce(&mut Task, &Connection) -> Result<()>,
    {
        validate_owner(actor)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut task = load(&tx, id)?;
        f(&mut task, &tx)?;
        task.updated_at = now;
        tx.execute(
            "UPDATE tasks SET state=?2,owner=?3,updated_at=?4,payload=?5 WHERE id=?1",
            params![
                task.id,
                serde_json::to_string(&task.state)?,
                task.owner,
                now,
                serde_json::to_string(&task)?
            ],
        )?;
        record(&tx, &task, actor, action)?;
        tx.commit()?;
        Ok(task)
    }

    pub fn assign(&mut self, id: &str, owner: &str, now: i64) -> Result<Task> {
        self.update(id, owner, "assign", now, |task, _| {
            ensure!(
                matches!(task.state, State::Queued | State::Assigned),
                "task is not assignable"
            );
            task.owner = Some(owner.to_owned());
            task.state = State::Assigned;
            Ok(())
        })
    }

    pub fn start(&mut self, id: &str, owner: &str, lease: i64, now: i64) -> Result<Task> {
        ensure!(
            (2..=86400).contains(&lease),
            "lease-seconds must be 2..86400"
        );
        self.update(id, owner, "start", now, |task, connection| {
            owned(task, owner)?;
            ensure!(
                task.state == State::Assigned,
                "task must be assigned before starting"
            );
            ensure!(
                task.attempt < task.max_attempts,
                "task exhausted its attempt budget"
            );
            for dependency in &task.depends_on {
                ensure!(
                    load(connection, dependency)?.state == State::Succeeded,
                    "dependency {dependency} has not succeeded"
                );
            }
            task.attempt += 1;
            task.state = State::Running;
            task.lease_until = Some(now + lease);
            task.exit_code = None;
            task.detail = None;
            task.receipt = None;
            Ok(())
        })
    }

    pub fn heartbeat(
        &mut self,
        id: &str,
        owner: &str,
        attempt: u32,
        lease: i64,
        now: i64,
    ) -> Result<Task> {
        ensure!(
            (2..=86400).contains(&lease),
            "lease-seconds must be 2..86400"
        );
        self.update(id, owner, "heartbeat", now, |task, _| {
            leased(task, owner, attempt, now)?;
            task.lease_until = Some(now + lease);
            Ok(())
        })
    }

    pub fn finish(
        &mut self,
        id: &str,
        owner: &str,
        attempt: u32,
        result: Completion,
        now: i64,
    ) -> Result<Task> {
        self.update(id, owner, "finish", now, |task, _| {
            leased(task, owner, attempt, now)?;
            ensure!(
                matches!(result.state, State::Succeeded | State::Failed),
                "invalid completion state"
            );
            task.state = result.state;
            task.lease_until = None;
            task.exit_code = result.exit_code;
            task.detail = result.detail;
            task.receipt = result.receipt;
            Ok(())
        })
    }

    pub fn retry(&mut self, id: &str, owner: &str, now: i64) -> Result<Task> {
        self.update(id, owner, "retry", now, |task, _| {
            owned(task, owner)?;
            let expired =
                task.state == State::Running && task.lease_until.is_some_and(|t| t <= now);
            ensure!(
                task.state == State::Failed || expired,
                "retry requires a failed task or expired lease"
            );
            ensure!(
                task.attempt < task.max_attempts,
                "task exhausted its attempt budget"
            );
            task.state = State::Assigned;
            task.lease_until = None;
            task.detail = Some(
                if expired {
                    "recovered expired lease"
                } else {
                    "retry requested"
                }
                .into(),
            );
            Ok(())
        })
    }

    pub fn cancel(&mut self, id: &str, owner: &str, now: i64) -> Result<Task> {
        self.update(id, owner, "cancel", now, |task, _| {
            if task.owner.is_some() {
                owned(task, owner)?;
            }
            ensure!(
                !matches!(task.state, State::Succeeded | State::Cancelled),
                "task is already terminal"
            );
            task.state = State::Cancelled;
            task.lease_until = None;
            task.detail = Some("cancel requested".into());
            Ok(())
        })
    }
}

pub struct Completion {
    pub state: State,
    pub exit_code: Option<i32>,
    pub detail: Option<String>,
    pub receipt: Option<Receipt>,
}

fn owned(task: &Task, owner: &str) -> Result<()> {
    ensure!(
        task.owner.as_deref() == Some(owner),
        "task owner mismatch (expected {:?})",
        task.owner
    );
    Ok(())
}

fn leased(task: &Task, owner: &str, attempt: u32, now: i64) -> Result<()> {
    owned(task, owner)?;
    ensure!(
        task.state == State::Running && task.attempt == attempt,
        "task attempt is no longer running"
    );
    ensure!(
        task.lease_until.is_some_and(|t| t > now),
        "task lease expired; explicit retry is required"
    );
    Ok(())
}

fn record(connection: &Connection, task: &Task, actor: &str, action: &str) -> Result<()> {
    connection.execute(
        "INSERT INTO events(task_id,at,actor,action,attempt,detail) VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            task.id,
            task.updated_at,
            actor,
            action,
            task.attempt,
            task.detail
        ],
    )?;
    Ok(())
}

pub fn required(host: &Path) -> Result<Store> {
    match Store::open(host, false)? {
        Some(store) => Ok(store),
        None => bail!("no task database; create a task first"),
    }
}
