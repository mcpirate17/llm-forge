//! Render recorded receipt evidence. Never executes, seals, publishes, or approves.
#[path = "candidate_report/human.rs"]
mod human;
#[path = "candidate_report/junit.rs"]
mod junit;
#[path = "candidate_report/model.rs"]
mod model;
#[path = "candidate_report/sarif.rs"]
mod sarif;
#[cfg(test)]
#[path = "candidate_report/tests.rs"]
mod tests;

use anyhow::Result;
use std::io::{self, Write};

const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum Format {
    Human,
    Json,
    Sarif,
    Junit,
}

/// Digest verification establishes self-consistency, not execution or approval.
pub fn render(
    receipt: &crate::candidate_receipt::VerifiedReceipt,
    format: Format,
) -> Result<Vec<u8>> {
    let report = model::Report::parse(receipt)?;
    let mut out = Output::default();
    match format {
        Format::Human => human::render(&report, &mut out)?,
        Format::Json => {
            serde_json::to_writer_pretty(&mut out, receipt.payload())?;
            out.write_all(b"\n")?;
        }
        Format::Sarif => sarif::render(&report, &mut out)?,
        Format::Junit => junit::render(&report, &mut out)?,
    }
    Ok(out.bytes)
}

#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("candidate report exceeds 16 MiB"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn json(out: &mut Output, value: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer(out, value)?;
    Ok(())
}

fn validate_structure(
    value: &serde_json::Value,
    depth: usize,
    remaining: &mut usize,
) -> Result<()> {
    anyhow::ensure!(
        depth <= 32 && *remaining > 0,
        "receipt structure exceeds render limits"
    );
    *remaining -= 1;
    match value {
        serde_json::Value::Array(items) => {
            for value in items {
                validate_structure(value, depth + 1, remaining)?;
            }
        }
        serde_json::Value::Object(items) => {
            for value in items.values() {
                validate_structure(value, depth + 1, remaining)?;
            }
        }
        _ => {}
    }
    Ok(())
}
