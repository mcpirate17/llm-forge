//! One admitted native tokenizer worker; recovery remains inside private state.
use super::{policy::ContextBudget, Blocking};
use crate::{context_store::Store, context_window, tokenizer::Counter};
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Semaphore;

pub struct Contexts {
    worker: Option<Arc<Worker>>,
    slot: Arc<Semaphore>,
    pool: Blocking,
}
struct Worker {
    counter: Counter,
    store: Store,
    budget: ContextBudget,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Count {
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recover {
    sha256: String,
    offset: usize,
    limit: usize,
}

impl Contexts {
    pub fn new(budget: Option<&ContextBudget>, state: &Path, pool: Blocking) -> Result<Self> {
        let worker = budget
            .map(|budget| {
                Ok::<_, anyhow::Error>(Arc::new(Worker {
                    counter: Counter::load(&budget.tokenizer)?,
                    store: Store::create(&state.join("context-recovery"), budget.recovery_bytes)?,
                    budget: budget.clone(),
                }))
            })
            .transpose()?;
        Ok(Self {
            worker,
            slot: Arc::new(Semaphore::new(1)),
            pool,
        })
    }
    pub async fn call(&self, name: &str, input: &str) -> Result<Value> {
        let worker = self
            .worker
            .as_ref()
            .context("context tools lack an operator capability")?
            .clone();
        let slot = self.slot.clone().acquire_owned().await?;
        let (name, input) = (name.to_owned(), input.to_owned());
        self.pool
            .run(move || {
                let _slot = slot;
                worker.call(&name, &input)
            })
            .await
    }
}
impl Worker {
    fn call(&self, name: &str, input: &str) -> Result<Value> {
        match name {
            "context_count" => {
                let args: Count = serde_json::from_str(input)?;
                ensure!(
                    args.text.len() <= self.budget.max_input_bytes,
                    "context exceeds operator byte limit"
                );
                Ok(
                    json!({"tokenizer":self.counter.identity(),"tokens":self.counter.count(&args.text)?,
                    "input_bytes":args.text.len(),"scope":"exact supplied text; external chat template/envelope excluded"}),
                )
            }
            "context_compact" => {
                let request: context_window::Request = serde_json::from_str(input)?;
                ensure!(
                    request.max_tokens <= self.budget.max_tokens
                        && request.max_bytes <= self.budget.max_input_bytes
                        && request
                            .entries
                            .iter()
                            .map(|e| e.content.len() + 2)
                            .sum::<usize>()
                            <= self.budget.max_input_bytes,
                    "context exceeds operator token/byte limits"
                );
                serde_json::to_value(context_window::compact(
                    &self.counter,
                    &self.store,
                    &request,
                )?)
                .map_err(Into::into)
            }
            "context_recover" => {
                let args: Recover = serde_json::from_str(input)?;
                ensure!(
                    (1..=65536).contains(&args.limit),
                    "invalid recovery chunk limit"
                );
                let bytes = self.store.get(&args.sha256)?;
                ensure!(
                    args.offset <= bytes.len(),
                    "recovery offset beyond artifact"
                );
                let end = args.offset + args.limit.min(bytes.len() - args.offset);
                Ok(
                    json!({"sha256":args.sha256,"total_bytes":bytes.len(),"offset":args.offset,"next":end,
                    "eof":end==bytes.len(),"base64":STANDARD.encode(&bytes[args.offset..end])}),
                )
            }
            _ => anyhow::bail!("unknown context tool"),
        }
    }
}
