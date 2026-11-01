use super::*;
pub(super) use crate::json_canon::encoded_size as bytes;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) const MAX_BYTES: usize = 128 * 1024 * 1024;
const ERROR_RESERVE: usize = 64 * 1024;
pub(super) struct Quota {
    remaining: AtomicUsize,
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
    row["error"] = if plan.scheduled && !plan.supported() {
        json!("scheduled check has no native batch implementation")
    } else {
        Value::Null
    };
    if crate::candidate_builtin::supports(plan) {
        row["builtin"] = crate::candidate_builtin::metadata(plan);
    }
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
