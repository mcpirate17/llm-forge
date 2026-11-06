//! Native tool service: validation before dispatch, explicit operator capabilities,
//! bounded asynchronous sessions and one shared durable task journal.
#[path = "harness_catalog.rs"]
mod catalog;
#[path = "harness_cli.rs"]
pub mod cli;
#[path = "harness_context.rs"]
mod context;
#[path = "harness_journal.rs"]
mod journal;
#[path = "harness_mcp.rs"]
pub mod mcp;
#[path = "harness_outbound.rs"]
mod outbound;
#[path = "harness_policy.rs"]
pub mod policy;
#[path = "harness_search.rs"]
mod search;
#[path = "harness_source.rs"]
mod source;
#[path = "harness_state.rs"]
pub(crate) mod state;
#[path = "harness_terminal.rs"]
mod terminal;
#[path = "harness_test_report.rs"]
mod test_report;
#[path = "harness_transport.rs"]
pub mod transport;
#[path = "harness_workspace.rs"]
pub mod workspace;
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{watch, Semaphore};

pub struct Harness {
    policy: Arc<policy::Policy>,
    catalog: Value,
    registry: crate::tool_schema::ToolRegistry,
    grammar: String,
    terminals: Arc<terminal::Terminals>,
    journal: journal::Journal,
    pool: Blocking,
    search: search::Searches,
    context: context::Contexts,
    workspace: workspace::Workspace,
    outbound: Option<outbound::Outbound>,
    reports: Option<test_report::Reports>,
    fatal: watch::Receiver<Option<String>>,
    stopping: Arc<AtomicBool>,
}

impl Harness {
    pub(crate) fn new(
        policy: policy::Policy,
        state: state::State,
        parent: PathBuf,
        owner: &str,
    ) -> Result<Self> {
        let catalog = catalog::catalog(&policy);
        let registry =
            crate::tool_schema::ToolRegistry::from_json(&serde_json::to_string(&catalog)?)?;
        let pool = Blocking::new(4);
        let search = search::Searches::new(&policy, pool.clone())?;
        let context = context::Contexts::new(policy.context.as_ref(), &state.root, pool.clone())?;
        let workspace = workspace::Workspace::new(&policy, pool.clone());
        let grammar = registry.grammar();
        let journal = journal::Journal::open(&state.root, owner, Blocking::new(1))?;
        let (send, fatal) = watch::channel(None);
        let policy = Arc::new(policy);
        let stopping = Arc::new(AtomicBool::new(false));
        let outbound =
            outbound::Outbound::new(&policy, &state.root, parent.clone(), stopping.clone())?;
        let reports =
            test_report::Reports::new(&policy, &state.root, parent.clone(), stopping.clone())?;
        let terminals = Arc::new(terminal::Terminals::new(
            policy.clone(),
            Arc::new(state),
            parent,
            journal.clone(),
            pool.clone(),
            send,
            stopping.clone(),
        ));
        Ok(Self {
            policy,
            catalog,
            registry,
            grammar,
            terminals,
            journal,
            pool,
            search,
            context,
            workspace,
            outbound,
            reports,
            fatal,
            stopping,
        })
    }

    pub fn catalog(&self) -> &Value {
        &self.catalog
    }
    pub fn grammar(&self) -> &str {
        &self.grammar
    }

    pub async fn call(&self, input: &str, repair: bool) -> Result<Value> {
        let call = self.prepare_call(input, repair)?;
        let (_cancel, cancelled) = watch::channel(false);
        self.execute_call(&call, cancelled).await
    }

    fn prepare_call<'r, 'i>(
        &'r self,
        input: &'i str,
        repair: bool,
    ) -> Result<crate::tool_schema::ValidatedCall<'r, 'i>> {
        ensure!(
            !self.stopping.load(Ordering::Acquire),
            "harness is stopping"
        );
        ensure!(
            self.fatal.borrow().is_none(),
            "harness persistence/ownership failed"
        );
        self.registry
            .validate_call(input, repair)
            .context("invalid tool call")
    }

    async fn execute_call(
        &self,
        call: &crate::tool_schema::ValidatedCall<'_, '_>,
        cancelled: watch::Receiver<bool>,
    ) -> Result<Value> {
        let result = if call.name() == "python_test_report" {
            self.reports
                .as_ref()
                .context("test report capability unavailable")?
                .call(call.arguments(), cancelled)
                .await?
        } else {
            self.dispatch(call).await?
        };
        Ok(json!({"result":result,"repaired":call.repaired()}))
    }

    async fn dispatch(&self, call: &crate::tool_schema::ValidatedCall<'_, '_>) -> Result<Value> {
        let name = call.name();
        let args = call.arguments();
        match name {
            "context_count" | "context_compact" | "context_recover" | "context_prompt" => {
                self.context.call(name, args).await
            }
            "code_search" => self.search.call(args).await,
            "session_brief" | "workspace_recall" => self.workspace.call(name, args).await,
            "code_inspect" | "code_fold" | "code_replace" | "code_diff" | "code_restore" => {
                let name = name.to_owned();
                let input = args.to_owned();
                let policy = self.policy.clone();
                self.pool
                    .run(move || match name.as_str() {
                        "code_inspect" => source::inspect(&policy, &input, false),
                        "code_fold" => source::inspect(&policy, &input, true),
                        _ => source::edit(&policy, &name, &input),
                    })
                    .await
            }
            "terminal_spawn" => self.terminals.spawn(args).await,
            "terminal_read" | "terminal_write" | "terminal_resize" | "terminal_interrupt"
            | "terminal_close" | "terminal_info" => self.terminals.interact(name, args).await,
            "task_get" => {
                let args: Value = serde_json::from_str(args)?;
                let id = args["id"].as_str().context("missing id")?.to_owned();
                self.journal
                    .call(move |store| Ok(serde_json::to_value(store.get(&id)?)?))
                    .await
            }
            "task_list" => {
                let args: Value = serde_json::from_str(args)?;
                let limit = args
                    .get("limit")
                    .map_or(Ok(20), |v| v.as_u64().context("limit must be an integer"))?;
                self.journal
                    .call(move |store| Ok(serde_json::to_value(store.list(limit as usize)?)?))
                    .await
            }
            _ if self
                .outbound
                .as_ref()
                .is_some_and(|outbound| outbound.name() == name) =>
            {
                self.outbound
                    .as_ref()
                    .context("outbound unavailable")?
                    .call(args)
                    .await
            }
            _ => anyhow::bail!("registered tool lacks handler"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Blocking {
    slots: Arc<Semaphore>,
}
impl Blocking {
    fn new(capacity: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(capacity)),
        }
    }
    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let permit = self.slots.clone().acquire_owned().await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .context("native blocking worker failed")?
    }
}
