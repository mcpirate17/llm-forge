//! Bounded native CLI transport. This validates calls; it does not grant
//! permissions or execute a model-selected command.
use super::{CompiledSchema, ToolRegistry, ValidationError, MAX_INPUT_BYTES};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use serde_json::json;
use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct SchemaArgs {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Validate one JSON document, rejecting malformed or unregistered parameters.
    Validate {
        #[arg(long)]
        schema: PathBuf,
        /// Read stdin when omitted.
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long)]
        repair_fences: bool,
    },
    /// Generate structural GBNF; numeric bounds still require runtime validation.
    Grammar {
        #[arg(long, required_unless_present = "catalog", conflicts_with = "catalog")]
        schema: Option<PathBuf>,
        #[arg(long, required_unless_present = "schema")]
        catalog: Option<PathBuf>,
    },
    /// Compile a complete tool catalog once and validate bounded JSONL calls.
    /// Each line is {"name":"tool", "arguments":{...}}. No tools are executed.
    Serve {
        #[arg(long)]
        catalog: PathBuf,
    },
}

pub fn run(args: SchemaArgs) -> Result<u8> {
    let mut output = io::stdout().lock();
    match args.action {
        Action::Grammar { schema, catalog } => {
            let grammar = match (schema, catalog) {
                (Some(path), None) => CompiledSchema::from_json(&read_file(&path)?)?.grammar(),
                (None, Some(path)) => ToolRegistry::from_json(&read_file(&path)?)?.grammar(),
                _ => anyhow::bail!("provide exactly one of --schema or --catalog"),
            };
            output.write_all(grammar.as_bytes())?;
            Ok(0)
        }
        Action::Validate {
            schema,
            input,
            repair_fences,
        } => {
            let schema = CompiledSchema::from_json(&read_file(&schema)?)?;
            let input = match input {
                Some(path) => read_file(&path)?,
                None => read_text(io::stdin().lock())?,
            };
            match schema.validate_with_repair(&input, repair_fences) {
                Ok(valid) => {
                    // Validated bytes are embedded directly, preserving exact
                    // decimals and avoiding a second DOM parse/serialization.
                    writeln!(
                        output,
                        "{{\"valid\":true,\"repaired\":{},\"value\":{}}}",
                        valid.repaired, valid.json
                    )?;
                    Ok(0)
                }
                Err(error) => {
                    write_error(&mut output, error)?;
                    Ok(1)
                }
            }
        }
        Action::Serve { catalog } => {
            let registry = ToolRegistry::from_json(&read_file(&catalog)?)?;
            serve(&registry, &mut io::stdin().lock(), &mut output)?;
            Ok(0)
        }
    }
}

fn read_file(path: &Path) -> Result<String> {
    read_text(File::open(path).with_context(|| format!("open {}", path.display()))?)
}

fn read_text(reader: impl Read) -> Result<String> {
    let mut input = String::new();
    reader
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_string(&mut input)?;
    ensure!(
        input.len() <= MAX_INPUT_BYTES,
        "input exceeds {MAX_INPUT_BYTES} bytes"
    );
    Ok(input)
}

fn write_error(output: &mut impl Write, error: ValidationError) -> io::Result<()> {
    writeln!(
        output,
        "{}",
        json!({"valid":false,"error":{"code":error.code,"offset":error.offset}})
    )
}

fn serve(registry: &ToolRegistry, input: &mut impl BufRead, output: &mut impl Write) -> Result<()> {
    let mut buffer = Vec::with_capacity(8192);
    while let Some(overflow) = read_record(input, &mut buffer)? {
        let error = if overflow {
            Some(ValidationError {
                code: "input_limit",
                offset: MAX_INPUT_BYTES,
            })
        } else if let Err(error) = std::str::from_utf8(&buffer) {
            Some(ValidationError {
                code: "utf8",
                offset: error.valid_up_to(),
            })
        } else {
            None
        };
        if let Some(error) = error {
            write_error(output, error)?;
        } else {
            let text = std::str::from_utf8(&buffer)?;
            match registry.validate_call(text, false) {
                Ok(call) => writeln!(
                    output,
                    "{{\"valid\":true,\"repaired\":{},\"name\":{},\"arguments\":{}}}",
                    call.repaired(),
                    serde_json::to_string(call.name())?,
                    call.arguments()
                )?,
                Err(error) => write_error(output, error)?,
            }
        }
        output.flush()?;
    }
    Ok(())
}

/// Drain oversized records without allocating in proportion to attacker input.
fn read_record(input: &mut impl BufRead, buffer: &mut Vec<u8>) -> io::Result<Option<bool>> {
    buffer.clear();
    let mut overflow = false;
    let mut seen = false;
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            return Ok(seen.then_some(overflow));
        }
        seen = true;
        let newline = available.iter().position(|b| *b == b'\n');
        let len = newline.unwrap_or(available.len());
        let remaining = MAX_INPUT_BYTES - buffer.len();
        buffer.extend_from_slice(&available[..len.min(remaining)]);
        overflow |= len > remaining;
        input.consume(len + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(Some(overflow));
        }
    }
}
