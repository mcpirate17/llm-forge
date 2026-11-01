use super::*;
use std::{
    io,
    sync::atomic::{AtomicUsize, Ordering},
};

pub(super) const MAX_BYTES: usize = 128 * 1024 * 1024;
const ERROR_RESERVE: usize = 64 * 1024;
pub(super) struct Quota {
    remaining: AtomicUsize,
}

struct Counter {
    count: usize,
    limit: usize,
}
impl io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.count = self
            .count
            .checked_add(bytes.len())
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| io::Error::other("batch JSON output exceeds limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn bytes(value: &Value, limit: usize) -> Result<usize> {
    let mut counter = Counter { count: 0, limit };
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.count)
}
impl Quota {
    pub(super) fn new(common: &Value, plans: &[Arc<CheckPlan>], execute: bool) -> Result<Self> {
        // Reserve every error row in advance so an oversized successful result
        // still has an explicit, bounded failure at its original policy position.
        let mut reserved = bytes(common, MAX_BYTES)? + ERROR_RESERVE;
        for plan in plans {
            reserved = reserved
                .checked_add(bytes(&row(plan, execute), MAX_BYTES)? + ERROR_RESERVE)
                .context("batch output reservation overflow")?;
            ensure!(
                reserved <= MAX_BYTES,
                "batch metadata exceeds output budget"
            );
        }
        ensure!(
            reserved <= MAX_BYTES,
            "batch metadata exceeds output budget"
        );
        Ok(Self {
            remaining: AtomicUsize::new(MAX_BYTES - reserved),
        })
    }
    pub(super) fn retain(&self, value: &Value, plan: &CheckPlan, execute: bool) -> Result<()> {
        let extra = bytes(value, MAX_BYTES)?.saturating_sub(bytes(&row(plan, execute), MAX_BYTES)?);
        self.reserve(extra)
    }
    fn reserve(&self, extra: usize) -> Result<()> {
        self.remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                left.checked_sub(extra)
            })
            .map_err(|_| anyhow::anyhow!("batch retained output exceeds 128 MiB budget"))?;
        Ok(())
    }
}

pub(super) fn row(plan: &CheckPlan, execute: bool) -> Value {
    let mut row = plan.output(execute);
    row["check_id"] = json!(plan.id);
    row["error"] = if plan.scheduled && plan.external.is_none() {
        json!("scheduled check has no native batch implementation")
    } else {
        Value::Null
    };
    row
}

pub(super) fn error(plan: &CheckPlan, execute: bool, error: &anyhow::Error) -> Value {
    let mut row = row(plan, execute);
    let text = format!("{error:#}");
    // Six bytes per control character is the worst JSON encoding expansion.
    let mut end = text.len().min((ERROR_RESERVE - 1024) / 6);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    row["error"] = json!(if end < text.len() {
        format!("{} [truncated]", &text[..end])
    } else {
        text
    });
    row
}

#[cfg(test)]
#[path = "candidate_batch_output_tests.rs"]
mod tests;
