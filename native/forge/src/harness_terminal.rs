use super::{
    journal::Journal,
    policy::{self, Policy, WorkingDirectory},
    state, Blocking,
};
use crate::{
    cgroup::Cgroup,
    sandbox,
    terminal_session::{self, Phase, Session},
};
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Mutex};

struct Entry {
    fingerprint: String,
    session: Session,
    published: watch::Receiver<bool>,
}

pub struct Terminals {
    policy: Arc<Policy>,
    state: Arc<state::State>,
    parent: PathBuf,
    journal: Journal,
    pool: Blocking,
    entries: Mutex<BTreeMap<String, Entry>>,
    fatal: watch::Sender<Option<String>>,
    stopping: Arc<AtomicBool>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Spawn {
    id: String,
    program: String,
    arguments: Vec<String>,
    rows: Option<u16>,
    columns: Option<u16>,
    timeout_seconds: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Interaction {
    id: String,
    cursor: Option<u64>,
    limit: Option<usize>,
    wait_ms: Option<u64>,
    sequence: Option<u64>,
    base64: Option<String>,
    rows: Option<u16>,
    columns: Option<u16>,
}

impl Terminals {
    pub fn new(
        policy: Arc<Policy>,
        state: Arc<state::State>,
        parent: PathBuf,
        journal: Journal,
        pool: Blocking,
        fatal: watch::Sender<Option<String>>,
        stopping: Arc<AtomicBool>,
    ) -> Self {
        Self {
            policy,
            state,
            parent,
            journal,
            pool,
            entries: Mutex::new(BTreeMap::new()),
            fatal,
            stopping,
        }
    }

    pub async fn spawn(self: &Arc<Self>, input: &str) -> Result<Value> {
        let args: Spawn = serde_json::from_str(input)?;
        crate::task_store::validate_id(&args.id)?;
        policy::arguments(&args.arguments)?;
        let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&args)?));
        let mut entries = self.entries.lock().await;
        ensure!(
            !self.stopping.load(Ordering::Acquire),
            "harness is stopping"
        );
        if let Some(existing) = entries.get(&args.id) {
            ensure!(
                existing.fingerprint == fingerprint,
                "session id was already used with different arguments"
            );
            return Ok(json!({"id":args.id,"reused":true,"status":existing.session.status()}));
        }
        ensure!(
            entries.len() < self.policy.max_retained_sessions,
            "terminal replay capacity exhausted"
        );
        ensure!(
            entries
                .values()
                .filter(|e| e.session.status().phase == Phase::Running)
                .count()
                < self.policy.max_sessions,
            "active terminal capacity exhausted"
        );
        let program = self
            .policy
            .programs
            .get(&args.program)
            .context("unknown executable capability")?;
        let mut command = vec![program.executable.to_string_lossy().into_owned()];
        command.extend(program.fixed_arguments.clone());
        command.extend(args.arguments.clone());
        self.journal
            .start(args.id.clone(), command, 60, self.policy.max_tasks)
            .await?;
        let policy = self.policy.clone();
        let state = self.state.clone();
        let parent = self.parent.clone();
        let id = args.id.clone();
        let spawned = self
            .pool
            .run(move || launch(&policy, &state, &parent, args))
            .await;
        let (session, worker) = match spawned {
            Ok(value) => value,
            Err(error) => {
                self.journal
                    .fail(id, format!("launch failed: {error:#}"))
                    .await?;
                return Err(error);
            }
        };
        let status = session.status();
        let (published, receipt_ready) = watch::channel(false);
        entries.insert(
            id.clone(),
            Entry {
                fingerprint,
                session: session.clone(),
                published: receipt_ready,
            },
        );
        let owner = self.clone();
        let monitor_id = id.clone();
        tokio::spawn(async move {
            if let Err(error) = owner
                .monitor(monitor_id.clone(), session, worker, published)
                .await
            {
                owner
                    .fatal
                    .send_replace(Some(format!("session {monitor_id}: {error:#}")));
            }
        });
        Ok(json!({"id":id,"reused":false,"status":status}))
    }

    async fn monitor(
        &self,
        id: String,
        session: Session,
        worker: tokio::task::JoinHandle<Result<terminal_session::Receipt>>,
        published: watch::Sender<bool>,
    ) -> Result<()> {
        loop {
            tokio::select! {
                status=session.wait_complete()=>{status?;break},
                _=tokio::time::sleep(Duration::from_secs(20))=>self.journal.heartbeat(id.clone()).await?,
            }
        }
        let receipt = session
            .receipt()
            .await?
            .context("missing completed terminal receipt")?;
        self.journal.complete(id.clone(), receipt).await?;
        published.send_replace(true);
        worker.await.context("terminal worker panicked")??;
        self.entries.lock().await.remove(&id);
        Ok(())
    }

    pub async fn interact(&self, name: &str, input: &str) -> Result<Value> {
        let args: Interaction = serde_json::from_str(input)?;
        let session = self
            .entries
            .lock()
            .await
            .get(&args.id)
            .map(|e| e.session.clone());
        let Some(session) = session else {
            ensure!(
                matches!(name, "terminal_info" | "terminal_close"),
                "terminal is unknown or replay retention ended; use task_get for durable history"
            );
            let task = self.journal.call(move |store| store.get(&args.id)).await?;
            ensure!(
                !matches!(task.state, crate::task_store::State::Running),
                "terminal lost live ownership"
            );
            return Ok(json!({"task":task,"replay_available":false}));
        };
        match name {
            "terminal_read" => read(&session, args).await,
            "terminal_write" => Ok(serde_json::to_value(
                session
                    .write(
                        args.sequence.context("sequence required")?,
                        STANDARD.decode(args.base64.context("base64 required")?)?,
                        Duration::from_millis(args.wait_ms.unwrap_or(100)),
                    )
                    .await?,
            )?),
            "terminal_resize" => Ok(serde_json::to_value(
                session
                    .resize(
                        args.rows.context("rows required")?,
                        args.columns.context("columns required")?,
                    )
                    .await?,
            )?),
            "terminal_interrupt" => Ok(serde_json::to_value(session.interrupt().await?)?),
            "terminal_close" => Ok(serde_json::to_value(session.close().await?)?),
            "terminal_info" => Ok(
                json!({"status":session.status(),"receipt":session.receipt().await?,"replay_available":true}),
            ),
            _ => anyhow::bail!("unregistered terminal handler"),
        }
    }

    pub async fn close_all(&self) -> Result<()> {
        let sessions: Vec<_> = self
            .entries
            .lock()
            .await
            .values()
            .map(|e| (e.session.clone(), e.published.clone()))
            .collect();
        let mut workers = tokio::task::JoinSet::new();
        for (session, mut receipt) in sessions {
            workers.spawn(async move {
                session.close().await?;
                receipt
                    .wait_for(|ready| *ready)
                    .await
                    .context("receipt publication failed")?;
                Ok::<_, anyhow::Error>(())
            });
        }
        let mut errors = Vec::new();
        while let Some(result) = workers.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => errors.push(format!("{error:#}")),
                Err(error) => errors.push(error.to_string()),
            }
        }
        ensure!(
            errors.is_empty(),
            "session shutdown failures: {}",
            errors.join("; ")
        );
        Ok(())
    }
}

async fn read(session: &Session, args: Interaction) -> Result<Value> {
    let cursor = args.cursor.context("cursor required")?;
    let wait = args.wait_ms.unwrap_or(0);
    if wait > 0 && session.status().output_end == cursor && session.status().phase == Phase::Running
    {
        // A long-poll deadline is a normal empty read, not an execution failure.
        session
            .wait_output(cursor, Duration::from_millis(wait))
            .await?;
    }
    let chunk = session.read(cursor, args.limit.unwrap_or(16384)).await?;
    Ok(
        json!({"start":chunk.start,"next":chunk.next,"end":chunk.end,"evicted":chunk.evicted,
        "base64":STANDARD.encode(chunk.bytes),"status":session.status()}),
    )
}

fn launch(
    policy: &Policy,
    state: &state::State,
    parent: &std::path::Path,
    args: Spawn,
) -> Result<(
    Session,
    tokio::task::JoinHandle<Result<terminal_session::Receipt>>,
)> {
    let program = policy
        .programs
        .get(&args.program)
        .context("missing executable capability")?;
    let directory = state.directory(&args.id)?;
    let scratch = directory.join("scratch");
    std::fs::DirBuilder::new().mode(0o700).create(&scratch)?;
    let mut paths = policy.read_only.clone();
    paths.push(policy.source_root.clone());
    paths.push(program.executable.clone());
    let sandbox = sandbox::Sandbox::prepare(sandbox::Policy {
        read_only: paths,
        scratch: scratch.clone(),
        working_directory: match program.working_directory {
            WorkingDirectory::Source => policy.source_root.clone(),
            WorkingDirectory::Scratch => scratch,
        },
        environment: program.environment.clone(),
    })?;
    let mut plan = sandbox.command(&program.executable)?;
    plan.command()
        .args(&program.fixed_arguments)
        .args(&args.arguments);
    state::write_new(
        &directory.join("sandbox.json"),
        &serde_json::to_vec_pretty(sandbox.report())?,
    )?;
    let group = Cgroup::create(parent, policy.session)?;
    terminal_session::spawn(
        plan,
        group,
        state::log(&directory.join("output.log"))?,
        terminal_session::Config {
            rows: args.rows.unwrap_or(24),
            columns: args.columns.unwrap_or(80),
            timeout: Duration::from_secs(args.timeout_seconds.unwrap_or(policy.session_seconds)),
            replay_bytes: policy.replay_bytes,
            log_bytes: policy.log_bytes,
            completed_retention: Duration::from_secs(policy.retention_seconds),
        },
    )
}
