//! Current-only warm native binding status measurement; no baseline speedup claim.
use anyhow::Result;
use conductor_native::branch_policy_runtime as core;
use std::{fs, hint::black_box, time::Instant};
fn main() -> Result<()> {
    let owned = tempfile::tempdir()?;
    let root = owned.path().canonicalize()?;
    git2::Repository::init(&root)?;
    for index in 0..32 {
        core::bind(
            &root,
            &format!("sol/topic-{index}-20261102"),
            &format!("claim-{index}"),
            "sol",
            "2026-11-02T06:00:00+00:00",
        )?;
    }
    let before = fs::read(core::binding_store_path(&root)?)?;
    let mut samples = Vec::new();
    for sample in 0..12 {
        let start = Instant::now();
        for _ in 0..20 {
            let row = core::status(&root, "2026-11-02T12:00:00+00:00")?;
            anyhow::ensure!(
                row["bindings"]
                    .as_array()
                    .is_some_and(|rows| rows.len() == 32),
                "invalid benchmark fixture"
            );
            black_box(row);
        }
        if sample > 0 {
            samples.push(start.elapsed().as_secs_f64() * 1e6 / 20.0);
        }
    }
    anyhow::ensure!(
        fs::read(core::binding_store_path(&root)?)? == before,
        "status changed binding bytes"
    );
    println!(
        "{}",
        serde_json::json!({"scope":"current-only warm in-process; no baseline or speedup claim","bindings":32,"source_bytes":before.len(),"warmup_batches":1,"samples_us_per_operation":samples,"operations_per_batch":20,"excludes":"fixture setup, CLI startup and final output rendering"})
    );
    Ok(())
}
