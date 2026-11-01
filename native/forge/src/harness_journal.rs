//! Reuse the task journal; all SQLite and filesystem work runs off the reactor.
use super::{state, Blocking};
use crate::task_store::{Completion, Receipt, State, Store, Task};
use crate::terminal_session::{Phase, Receipt as TerminalReceipt};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Journal {
    store: Arc<Mutex<Store>>,
    pool: Blocking,
    owner: String,
    root: PathBuf,
}

impl Journal {
    pub fn open(root: &Path, owner: &str, pool: Blocking) -> Result<Self> {
        let store =
            Store::open_path(&root.join("tasks.sqlite3"), true)?.context("missing journal")?;
        store.set_busy_timeout(std::time::Duration::from_secs(1))?;
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            pool,
            owner: owner.into(),
            root: root.into(),
        })
    }

    pub async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        self.pool
            .run(move || {
                f(&mut *store
                    .lock()
                    .map_err(|_| anyhow::anyhow!("task journal poisoned"))?)
            })
            .await
    }

    pub async fn heartbeat(&self, id: String) -> Result<()> {
        let owner = self.owner.clone();
        self.call(move |store| {
            store.heartbeat(&id, &owner, 1, 60, state::now()?)?;
            Ok(())
        })
        .await
    }

    pub async fn start(
        &self,
        id: String,
        command: Vec<String>,
        lease: i64,
        maximum: usize,
    ) -> Result<Task> {
        let owner = self.owner.clone();
        self.call(move |store| {
            let now = state::now()?;
            anyhow::ensure!(
                store.counts()?.values().sum::<i64>() < maximum as i64,
                "service task-history capacity exhausted; start a new bounded service"
            );
            store.create(
                Task {
                    id: id.clone(),
                    title: format!("PTY {}", command[0]),
                    state: State::Queued,
                    owner: None,
                    command,
                    depends_on: Vec::new(),
                    claim_id: None,
                    session_id: Some(id.clone()),
                    message_id: None,
                    attempt: 0,
                    max_attempts: 1,
                    lease_until: None,
                    created_at: now,
                    updated_at: now,
                    exit_code: None,
                    detail: None,
                    receipt: None,
                },
                &owner,
            )?;
            store.assign(&id, &owner, now)?;
            store.start(&id, &owner, lease, now)
        })
        .await
    }

    pub async fn complete(&self, id: String, result: TerminalReceipt) -> Result<()> {
        let owner = self.owner.clone();
        let path = self.root.join(&id).join("receipt.json");
        self.call(move |store| {
            let hash = state::write_new(&path, &serde_json::to_vec_pretty(&result)?)?;
            let state =
                if result.status.phase == Phase::Exited && result.status.exit_code == Some(0) {
                    State::Succeeded
                } else {
                    State::Failed
                };
            store.finish(
                &id,
                &owner,
                1,
                Completion {
                    state,
                    exit_code: result.status.exit_code,
                    detail: result.error,
                    receipt: Some(Receipt { path, sha256: hash }),
                },
                state::now()?,
            )?;
            Ok(())
        })
        .await
    }

    pub async fn fail(&self, id: String, error: String) -> Result<()> {
        let owner = self.owner.clone();
        self.call(move |store| {
            store.finish(
                &id,
                &owner,
                1,
                Completion {
                    state: State::Failed,
                    exit_code: None,
                    detail: Some(error),
                    receipt: None,
                },
                state::now()?,
            )?;
            Ok(())
        })
        .await
    }
}
