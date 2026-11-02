use super::{MAX_BASELINE, MAX_ROWS};
use anyhow::{ensure, Context, Result};
use conductor_native::radon_complexity::Block;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fmt::Write, path::Path};
fn flagged(block: &Block, minimum: &str) -> bool {
    block.rank >= minimum
}
fn describe(block: &Block) -> String {
    format!(
        "{:3} {} {}:{} {} {}",
        block.complexity, block.rank, block.path, block.line, block.kind, block.name
    )
}
pub(super) fn print(blocks: &[Block], minimum: &str) {
    let counts: Vec<_> = ["A", "B", "C", "D", "E", "F"]
        .iter()
        .map(|r| format!("{r}={}", blocks.iter().filter(|b| b.rank == *r).count()))
        .collect();
    let flagged: Vec<_> = blocks.iter().filter(|b| flagged(b, minimum)).collect();
    println!("Total blocks: {}", blocks.len());
    println!("Rank counts: {}", counts.join(", "));
    println!("{minimum}-F blocks: {}", flagged.len());
    for block in flagged.iter().take(50) {
        println!("{}", describe(block));
    }
}
fn load(path: &Path) -> Result<BTreeMap<String, i64>> {
    let raw = crate::bounded_file::read(path, MAX_BASELINE).with_context(|| format!("complexity baseline not found or unreadable: {}. Name the host's own file with [tool.conductor].radon_complexity_baseline, $CONDUCTOR_RADON_BASELINE or --baseline", path.display()))?;
    let value: Value =
        serde_json::from_slice(&raw).context("malformed complexity baseline JSON")?;
    let object = value
        .as_object()
        .context("complexity baseline must be an object")?;
    let empty = Vec::new();
    let entries = match object.get("findings") {
        None => &empty,
        Some(value) => value
            .as_array()
            .context("baseline findings must be a list")?,
    };
    ensure!(
        entries.len() <= MAX_ROWS,
        "complexity baseline exceeds 65536 rows"
    );
    let mut baseline = BTreeMap::new();
    for item in entries {
        let key = item
            .get("key")
            .and_then(Value::as_str)
            .context("baseline key must be a string")?;
        // No coercive malformed rows: strict numeric admission is explicit.
        let score = item
            .get("complexity")
            .and_then(Value::as_i64)
            .context("baseline complexity must be an integer fitting i64")?;
        baseline
            .entry(key.into())
            .and_modify(|current: &mut i64| *current = (*current).min(score))
            .or_insert(score);
    }
    Ok(baseline)
}
pub(super) fn check(path: &Path, blocks: &[Block], minimum: &str) -> Result<u8> {
    let baseline = load(path)?;
    let current: Vec<_> = blocks.iter().filter(|b| flagged(b, minimum)).collect();
    let new: Vec<_> = current
        .iter()
        .filter(|b| !baseline.contains_key(&b.key))
        .collect();
    let worsened: Vec<_> = current
        .iter()
        .filter(|b| {
            baseline
                .get(&b.key)
                .is_some_and(|old| b.complexity as i64 > *old)
        })
        .collect();
    let mut best = BTreeMap::new();
    for block in blocks.iter().filter(|b| baseline.contains_key(&b.key)) {
        best.entry(block.key.as_str())
            .and_modify(|score: &mut usize| *score = (*score).min(block.complexity))
            .or_insert(block.complexity);
    }
    let improved: Vec<_> = best
        .into_iter()
        .filter_map(|(key, score)| {
            let old = baseline[key];
            ((score as i64) < old).then_some((key, old, score))
        })
        .collect();
    if !improved.is_empty() {
        println!(
            "{} grandfathered blocks improved; run refresh-baseline:",
            improved.len()
        );
        for (key, old, now) in improved.iter().take(50) {
            println!("  {old} -> {now}  {key}");
        }
    }
    if new.is_empty() && worsened.is_empty() {
        println!("Complexity ratchet passed: no new or worsened {minimum}-F blocks.");
        return Ok(0);
    }
    if !new.is_empty() {
        println!(
            "Complexity ratchet failed: {} new {minimum}-F blocks.",
            new.len()
        );
        for block in new.iter().take(50) {
            println!("{}", describe(block));
        }
    }
    if !worsened.is_empty() {
        println!(
            "Complexity ratchet failed: {} grandfathered blocks worsened.",
            worsened.len()
        );
        for block in worsened.iter().take(50) {
            println!(
                "{}  ({} -> {})",
                describe(block),
                baseline[&block.key],
                block.complexity
            );
        }
    }
    Ok(1)
}
/// Python json.dumps(indent=2,sort_keys=True,ensure_ascii=True) for admitted JSON.
fn ascii_pretty(mut value: Value) -> Result<Vec<u8>> {
    value.sort_all_objects();
    let pretty = serde_json::to_string_pretty(&value)?;
    let mut result = String::new();
    for ch in pretty.chars() {
        if (ch as u32) < 0x7f {
            result.push(ch);
        } else {
            let mut units = [0; 2];
            for unit in ch.encode_utf16(&mut units) {
                write!(result, "\\u{unit:04x}")?;
            }
        }
        ensure!(
            result.len() < MAX_BASELINE,
            "complexity baseline exceeds 16 MiB output budget"
        );
    }
    result.push('\n');
    Ok(result.into_bytes())
}
pub(super) fn write(
    path: &Path,
    blocks: &[Block],
    minimum: &str,
    paths: &[String],
    excludes: &[String],
) -> Result<()> {
    let selected: Vec<_> = blocks.iter().filter(|b| flagged(b, minimum)).collect();
    let payload =
        json!({"minimum_rank":minimum,"paths":paths,"excludes":excludes,"findings":selected});
    let bytes = ascii_pretty(payload)?;
    // Reuse bounded native atomic publication; never truncate a FIFO or partial baseline.
    crate::candidate_report_store::publish(path, &bytes)
}
