//! GPU guest selection, bounded and conservative when the probe is unavailable.
use super::config::Env;
use anyhow::{ensure, Result};
use std::process::Command;
use std::time::Duration;

pub fn choose_num_gpu(env: &Env) -> Result<u32> {
    choose_with_timeout(env, Duration::from_secs(2))
}

pub(super) fn choose_with_timeout(env: &Env, timeout: Duration) -> Result<u32> {
    if let Some(raw) = env.get("CPU_EMBED_NUM_GPU") {
        let value: u32 = raw
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("CPU_EMBED_NUM_GPU must be 0 or 99"))?;
        ensure!(matches!(value, 0 | 99), "CPU_EMBED_NUM_GPU must be 0 or 99");
        return Ok(value);
    }
    if timeout.is_zero() {
        return Ok(0);
    }
    let mut command = Command::new("nvidia-smi");
    command.args([
        "--query-compute-apps=process_name,used_memory",
        "--format=csv,noheader,nounits",
    ]);
    let output = match crate::process_probe::run(
        &mut command,
        timeout.min(Duration::from_secs(2)),
        256 * 1024,
    ) {
        Ok(output) if output.status.success() => output,
        _ => return Ok(0),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut busy = 0_u64;
    for line in text.lines() {
        let mut parts = line.split(',').map(str::trim);
        let (Some(name), Some(memory)) = (parts.next(), parts.next()) else {
            continue;
        };
        if ["gnome-remote-desktop", "Xorg", "gnome-shell", "ollama"]
            .iter()
            .any(|s| name.contains(s))
        {
            continue;
        }
        if let Ok(memory) = memory.parse::<u64>() {
            busy = busy.saturating_add(memory);
        }
    }
    Ok(if busy >= 2048 { 0 } else { 99 })
}
