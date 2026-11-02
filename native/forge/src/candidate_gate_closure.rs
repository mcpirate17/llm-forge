//! Live-index/live-source import closure, replayed before gate disposition.
use super::types::Phase;
use crate::candidate_source::Source;
use anyhow::{ensure, Result};
use conductor_native::dead_tests::bounded::{self, Scan};
use serde_json::json;
pub(super) struct Closure {
    tracked: Vec<String>,
    scan: Scan,
    pub phase: Phase,
}
impl Closure {
    pub fn collect(source: &Source) -> Result<Self> {
        let tracked = source.tracked_index_paths()?;
        let scan = bounded::scan(source.root(), &tracked)?;
        let count = scan.records.len();
        let names = scan
            .records
            .iter()
            .take(5)
            .map(|row| row.module.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let detail = if count == 0 {
            "no untracked import dependencies".into()
        } else {
            format!("{count} untracked module(s) are import-reachable from tracked code: {names}")
        };
        let phase = Phase::new(
            "clean-clone",
            count == 0,
            detail,
            json!({"count":count,"source":"live-index and live working tree"}),
        );
        Ok(Self {
            tracked,
            scan,
            phase,
        })
    }
    pub fn verify(&self, source: &Source) -> Result<()> {
        ensure!(
            source.tracked_index_paths()? == self.tracked,
            "live index paths changed during gate"
        );
        ensure!(
            bounded::scan(source.root(), &self.tracked)? == self.scan,
            "live import closure sources or lookups changed during gate"
        );
        Ok(())
    }
}
