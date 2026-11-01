//! Independent batch owner for already-authorized stage callbacks. A dropped
//! client signals cancellation; runtime shutdown and blocked kernel I/O still
//! require an external process watchdog.
use crate::confined_execution::{Cancellation, Completion, Execution};
use anyhow::{ensure, Context, Result};
use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::{
    sync::oneshot,
    task::{Id, JoinSet},
};

const MAX_ACTIVE: usize = 32;
struct Entry {
    completion: Completion,
    cancel: Arc<Cancellation>,
    dropped: bool,
}
struct Registry {
    next: u64,
    entries: BTreeMap<u64, Entry>,
    failures: Vec<String>,
    poisoned: bool,
}
pub struct Owners {
    inner: Mutex<Registry>,
    global: Arc<Cancellation>,
}
pub struct Guard {
    owners: Arc<Owners>,
    id: u64,
    finished: bool,
}
impl Owners {
    fn new(global: Arc<Cancellation>) -> Self {
        Self {
            global,
            inner: Mutex::new(Registry {
                next: 0,
                entries: BTreeMap::new(),
                failures: Vec::new(),
                poisoned: false,
            }),
        }
    }
    fn lock_registry(&self) -> std::sync::MutexGuard<'_, Registry> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(error) => {
                let mut guard = error.into_inner();
                if !guard.poisoned {
                    guard.poisoned = true;
                    guard.failures.push("batch owner registry poisoned".into());
                }
                guard
            }
        }
    }
    /// Caller must register immediately after start, before awaiting; if this
    /// fails, caller retains the Execution, cancels it, and awaits its cleanup.
    pub fn register(self: &Arc<Self>, execution: &Execution) -> Result<Guard> {
        let mut registry = self.lock_registry();
        ensure!(!registry.poisoned, "batch owner registry poisoned");
        prune_dropped(&mut registry);
        ensure!(
            registry.entries.len() < MAX_ACTIVE,
            "batch active execution registry full"
        );
        let id = registry.next;
        registry.next = registry
            .next
            .checked_add(1)
            .context("batch owner id overflow")?;
        registry.entries.insert(
            id,
            Entry {
                completion: execution.completion(),
                cancel: execution.cancellation(),
                dropped: false,
            },
        );
        if self.global.is_cancelled() {
            execution.cancellation().cancel()?;
        }
        Ok(Guard {
            owners: self.clone(),
            id,
            finished: false,
        })
    }
    fn cancel_all(&self) -> Result<()> {
        let registry = self.lock_registry();
        let mut errors = Vec::new();
        for entry in registry.entries.values() {
            if let Err(error) = entry.cancel.cancel() {
                errors.push(format!("{error:#}"));
            }
        }
        ensure!(
            errors.is_empty(),
            "batch cancellation wakeup failed: {}",
            errors.join("; ")
        );
        Ok(())
    }
    async fn finish_all(&self) -> Result<()> {
        let (entries, poisoned) = {
            let registry = self.lock_registry();
            (
                registry
                    .entries
                    .iter()
                    .map(|(id, entry)| (*id, entry.completion.clone()))
                    .collect::<Vec<_>>(),
                registry.poisoned,
            )
        };
        let mut errors = Vec::new();
        if poisoned {
            if let Err(error) = self.cancel_all() {
                errors.push(format!("{error:#}"));
            }
        }
        for (id, mut completion) in entries {
            match completion.wait().await {
                Ok(terminal) if terminal.cleanup_complete => {}
                Ok(_) => errors.push(format!("execution {id} cleanup incomplete")),
                Err(error) => errors.push(format!("execution {id}: {error:#}")),
            }
        }
        let mut registry = self.lock_registry();
        errors.append(&mut registry.failures);
        registry.entries.clear();
        ensure!(
            errors.is_empty(),
            "batch cleanup unproven: {}",
            errors.join("; ")
        );
        Ok(())
    }
}
impl Guard {
    pub fn finish(mut self) -> Result<()> {
        let mut registry = self.owners.lock_registry();
        let terminal = registry
            .entries
            .get(&self.id)
            .context("batch execution owner missing")?
            .completion
            .finished()
            .context("execution has no terminal cleanup evidence")?;
        registry.entries.remove(&self.id);
        self.finished = true;
        if !terminal.cleanup_complete {
            registry
                .failures
                .push(format!("execution {} cleanup incomplete", self.id));
            anyhow::bail!("execution cleanup incomplete");
        }
        Ok(())
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut registry = self.owners.lock_registry();
        if let Some(entry) = registry.entries.get_mut(&self.id) {
            entry.dropped = true;
            if let Err(error) = entry.cancel.cancel() {
                registry
                    .failures
                    .push(format!("batch guard cancellation failed: {error:#}"));
            }
        }
    }
}

fn prune_dropped(registry: &mut Registry) {
    let completed: Vec<_> = registry
        .entries
        .iter()
        .filter_map(|(id, entry)| {
            (entry.dropped)
                .then(|| entry.completion.finished().map(|t| (*id, t)))
                .flatten()
        })
        .collect();
    for (id, terminal) in completed {
        registry.entries.remove(&id);
        if !terminal.cleanup_complete {
            registry
                .failures
                .push(format!("execution {id} cleanup incomplete"));
        }
    }
}

pub struct Batch<O> {
    cancellation: Arc<Cancellation>,
    receiver: Option<oneshot::Receiver<Result<Vec<Result<O>>>>>,
    completed: bool,
}
impl<O> Batch<O> {
    pub fn cancellation(&self) -> Arc<Cancellation> {
        self.cancellation.clone()
    }
    pub async fn wait(mut self) -> Result<Vec<Result<O>>> {
        let result = self
            .receiver
            .take()
            .context("batch already awaited")?
            .await
            .context("batch owner stopped before cleanup")?;
        self.completed = true;
        result
    }
}
impl<O> Drop for Batch<O> {
    fn drop(&mut self) {
        if !self.completed {
            if let Err(error) = self.cancellation.cancel() {
                eprintln!("forge: batch cancellation failed: {error:#}");
            }
        }
    }
}

pub fn start<J, O, F, Fut>(jobs: Vec<J>, parallelism: usize, run: F) -> Result<Batch<O>>
where
    J: Send + 'static,
    O: Send + 'static,
    F: Fn(J, Arc<Cancellation>, Arc<Owners>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<O>> + Send + 'static,
{
    tokio::runtime::Handle::try_current().context("batch requires a Tokio reactor")?;
    ensure!((1..=256).contains(&jobs.len()), "batch jobs must be 1..256");
    ensure!(
        (1..=MAX_ACTIVE).contains(&parallelism),
        "batch parallelism must be 1..32"
    );
    let cancellation = Arc::new(Cancellation::new()?);
    let cancel = cancellation.clone();
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let result = coordinate(jobs, parallelism, run, cancel).await;
        if let Err(result) = sender.send(result) {
            match result {
                Err(error) => eprintln!("forge: departed batch client; cleanup failed: {error:#}"),
                Ok(results) => {
                    for error in results.into_iter().filter_map(Result::err) {
                        eprintln!("forge: departed batch client; job failed: {error:#}");
                    }
                }
            }
        }
    });
    Ok(Batch {
        cancellation,
        receiver: Some(receiver),
        completed: false,
    })
}
struct Running<O> {
    tasks: JoinSet<Result<O>>,
    ids: BTreeMap<Id, usize>,
}
impl<O: Send + 'static> Running<O> {
    fn new() -> Self {
        Self {
            tasks: JoinSet::new(),
            ids: BTreeMap::new(),
        }
    }
    fn spawn<J, F, Fut>(
        &mut self,
        index: usize,
        job: J,
        run: F,
        cancel: Arc<Cancellation>,
        owners: Arc<Owners>,
    ) where
        J: Send + 'static,
        F: Fn(J, Arc<Cancellation>, Arc<Owners>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<O>> + Send + 'static,
    {
        let handle = self
            .tasks
            .spawn(async move { run(job, cancel, owners).await });
        self.ids.insert(handle.id(), index);
    }
}
async fn coordinate<J, O, F, Fut>(
    jobs: Vec<J>,
    parallelism: usize,
    run: F,
    cancel: Arc<Cancellation>,
) -> Result<Vec<Result<O>>>
where
    J: Send + 'static,
    O: Send + 'static,
    F: Fn(J, Arc<Cancellation>, Arc<Owners>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<O>> + Send + 'static,
{
    let count = jobs.len();
    let mut queue: VecDeque<_> = jobs.into_iter().enumerate().collect();
    let mut results: Vec<Option<Result<O>>> = std::iter::repeat_with(|| None).take(count).collect();
    let owners = Arc::new(Owners::new(cancel.clone()));
    let mut running = Running::new();
    let mut cancelling = false;
    let mut errors = Vec::new();
    while !queue.is_empty() || !running.tasks.is_empty() {
        if cancel.is_cancelled() && !cancelling {
            cancelling = true;
            if let Err(error) = owners.cancel_all() {
                errors.push(format!("{error:#}"));
            }
            for (index, _) in queue.drain(..) {
                results[index] = Some(Err(anyhow::anyhow!("batch cancelled before job start")));
            }
        }
        while !cancelling && !cancel.is_cancelled() && running.tasks.len() < parallelism {
            let Some((index, job)) = queue.pop_front() else {
                break;
            };
            running.spawn(index, job, run.clone(), cancel.clone(), owners.clone());
        }
        if running.tasks.is_empty() {
            if cancel.is_cancelled() && !cancelling {
                continue;
            }
            break;
        }
        let joined = if cancelling {
            running.tasks.join_next_with_id().await
        } else {
            tokio::select! {
                result = running.tasks.join_next_with_id() => result,
                result = cancel.cancelled() => { if let Err(error) = result { errors.push(format!("cancellation: {error:#}")); let _ = cancel.cancel(); } continue; }
            }
        };
        if let Some(joined) = joined {
            let (id, result) = match joined {
                Ok((id, result)) => (id, result),
                Err(error) => {
                    let id = error.id();
                    if let Err(wake) = cancel.cancel() {
                        errors.push(format!("cancellation: {wake:#}"));
                    }
                    (id, Err(anyhow::anyhow!("batch worker failed: {error}")))
                }
            };
            if let Some(index) = running.ids.remove(&id) {
                results[index] = Some(result);
            } else {
                errors.push("unknown batch worker id".into());
                if let Err(error) = cancel.cancel() {
                    errors.push(format!("cancellation: {error:#}"));
                }
            }
        }
    }
    if let Err(error) = owners.finish_all().await {
        errors.push(format!("{error:#}"));
    }
    if !errors.is_empty() {
        for (index, value) in results.iter().enumerate() {
            if let Some(Err(error)) = value {
                errors.push(format!("job {index}: {}", bounded_error(error)));
            }
        }
        anyhow::bail!("batch owner failure: {}", errors.join("; "));
    }
    results
        .into_iter()
        .map(|value| value.context("batch job result missing"))
        .collect()
}

// Keep per-job context including recovery paths, without unbounded output.
fn bounded_error(error: &anyhow::Error) -> String {
    use std::fmt::Write;
    struct Capped {
        text: String,
        truncated: bool,
    }
    impl std::fmt::Write for Capped {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            let left = 4080_usize.saturating_sub(self.text.len());
            let mut end = value.len().min(left);
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            self.text.push_str(&value[..end]);
            self.truncated |= end < value.len();
            Ok(())
        }
    }
    let mut result = Capped {
        text: String::new(),
        truncated: false,
    };
    let _ = write!(&mut result, "{error:#}");
    if result.truncated {
        result.text.push_str("...[truncated]");
    }
    result.text
}

#[cfg(test)]
#[path = "batch_execution_tests.rs"]
mod tests;
