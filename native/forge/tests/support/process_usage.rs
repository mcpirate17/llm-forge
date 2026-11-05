//! Point observations only; CPU uses kernel ticks and RSS/HWM are not tree peaks.
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::fs;
pub fn process(pid: u32) -> Value {
    let observation = || -> Result<Value> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .context("invalid proc stat")?
            .1
            .split_whitespace()
            .collect();
        ensure!(fields.len() > 19, "short proc stat");
        let ticks = fields[11]
            .parse::<u64>()?
            .checked_add(fields[12].parse()?)
            .context("CPU tick overflow")?;
        let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
        let rss = |prefix| -> Option<u64> {
            status
                .lines()
                .find_map(|line| line.strip_prefix(prefix))?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()?
                .checked_mul(1024)
        };
        Ok(
            json!({"available":true,"pid":pid,"start_ticks":fields[19].parse::<u64>()?,
            "cpu_ticks":ticks,"rss_bytes":rss("VmRSS:"),"hwm_bytes":rss("VmHWM:")}),
        )
    };
    observation()
        .unwrap_or_else(|error| json!({"available":false,"pid":pid,"reason":format!("{error:#}")}))
}
