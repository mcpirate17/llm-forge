//! Native token-budgeted context with atomic exchange retention and exact recovery.
use crate::{
    context_diagnostics,
    context_store::Store,
    tokenizer::{Counter, Identity, MAX_TEXT_BYTES},
};
use anyhow::{ensure, Context, Result};
use conductor_native::syntax::{self, Language};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Instructions,
    Policy,
    Approval,
    Error,
    Text,
    Code,
    Log,
    Diff,
    ToolCall,
    ToolResult,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub kind: Kind,
    pub content: String,
    #[serde(default)]
    pub protected: bool,
    /// Contiguous entries in one exchange are retained or omitted together.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exchange: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub entries: Vec<Entry>,
    pub max_tokens: usize,
    pub max_bytes: usize,
    pub reserved_tokens: usize,
    /// Protect the newest complete groups, in addition to explicit instructions/errors.
    pub keep_recent: usize,
}
#[derive(Serialize)]
pub struct Output {
    pub text: String,
    pub tokenizer: Identity,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub reserved_tokens: usize,
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub protected_overflow: bool,
    pub recovery_sha256: Option<String>,
    pub omitted_ids: Vec<String>,
    pub reductions: Vec<Reduction>,
}
#[derive(Serialize)]
pub struct Reduction {
    pub id: String,
    pub method: &'static str,
    pub before_bytes: usize,
    pub after_bytes: usize,
    pub diagnostic_errors: usize,
    pub diagnostic_warnings: usize,
    pub error: Option<String>,
}

struct Unit {
    range: std::ops::Range<usize>,
    required: bool,
    kept: bool,
    tokens: usize,
}

impl Request {
    fn units(&self) -> Result<Vec<Unit>> {
        ensure!(self.entries.len() <= 256, "context entry limit is 256");
        ensure!(
            (1..=MAX_TEXT_BYTES).contains(&self.max_tokens)
                && self.reserved_tokens < self.max_tokens
                && (128..=MAX_TEXT_BYTES).contains(&self.max_bytes),
            "invalid context token/byte budget"
        );
        let mut ids = HashSet::new();
        let mut exchanges = HashSet::new();
        let mut units: Vec<Unit> = Vec::new();
        let mut bytes = 0usize;
        let mut active = None;
        let mut called = false;
        for (i, entry) in self.entries.iter().enumerate() {
            ensure!(
                !entry.id.is_empty() && entry.id.len() <= 128 && ids.insert(&entry.id),
                "context IDs must be unique and 1..128 bytes"
            );
            bytes = bytes
                .checked_add(entry.content.len() + 2)
                .context("context size overflow")?;
            ensure!(
                bytes <= MAX_TEXT_BYTES - 512,
                "context exceeds bounded tokenizer input"
            );
            if entry.kind == Kind::Code {
                ensure!(
                    entry.language.is_some(),
                    "code context needs an explicit language"
                );
            }
            if matches!(entry.kind, Kind::ToolCall | Kind::ToolResult) {
                ensure!(
                    entry.exchange.is_some(),
                    "tool messages require an exchange ID"
                );
            }
            let same = entry.exchange.is_some() && entry.exchange.as_ref() == active;
            if !same {
                if let Some(exchange) = &entry.exchange {
                    ensure!(
                        !exchange.is_empty() && exchange.len() <= 128 && exchanges.insert(exchange),
                        "exchange IDs must be bounded and contiguous"
                    );
                }
                called = false;
                units.push(Unit {
                    range: i..i + 1,
                    required: false,
                    kept: true,
                    tokens: 0,
                });
            }
            if entry.kind == Kind::ToolCall {
                called = true;
            }
            ensure!(
                entry.kind != Kind::ToolResult || called,
                "tool result precedes its call"
            );
            active = entry.exchange.as_ref();
            let unit = units.last_mut().context("context group absent")?;
            unit.range.end = i + 1;
            unit.required |= entry.protected
                || matches!(
                    entry.kind,
                    Kind::Instructions | Kind::Policy | Kind::Approval | Kind::Error
                );
        }
        ensure!(
            self.keep_recent <= units.len(),
            "keep_recent exceeds context group count"
        );
        for unit in units.iter_mut().rev().take(self.keep_recent) {
            unit.required = true;
        }
        Ok(units)
    }
}

pub fn compact(counter: &Counter, store: &Store, request: &Request) -> Result<Output> {
    let mut units = request.units()?;
    let mut contents: Vec<String> = request.entries.iter().map(|e| e.content.clone()).collect();
    let input = contents.join("\n\n");
    let mut output = Output {
        input_tokens: counter.count(&input)?,
        input_bytes: input.len(),
        text: input,
        tokenizer: counter.identity().clone(),
        output_tokens: 0,
        output_bytes: 0,
        reserved_tokens: request.reserved_tokens,
        protected_overflow: false,
        recovery_sha256: None,
        omitted_ids: Vec::new(),
        reductions: Vec::new(),
    };
    output.output_tokens = output.input_tokens;
    if fits(request, &output) {
        return Ok(finish(output));
    }
    let encoded = serde_json::to_vec(
        &serde_json::json!({"version":1,"tokenizer":counter.identity(),"request":request}),
    )?;
    let hash = store
        .put(&encoded)
        .context("context not reduced because exact recovery could not be persisted")?;
    let marker = format!("[Context reduced; exact original: context recover --sha256 {hash}]");
    output.recovery_sha256 = Some(hash);
    reduce(request, &mut contents, &mut units, &mut output.reductions)?;
    for unit in &mut units {
        unit.tokens = counter.count(&contents[unit.range.clone()].join("\n\n"))?;
    }
    render(counter, &contents, &units, &marker, &mut output)?;
    // Independent group counts guide multi-group eviction; every final decision
    // is checked against the complete rendered text, including seams and marker.
    while !fits(request, &output) {
        let deficit_tokens =
            (output.output_tokens + request.reserved_tokens).saturating_sub(request.max_tokens);
        let deficit_bytes = output.text.len().saturating_sub(request.max_bytes);
        let (mut tokens, mut bytes, mut count) = (0, 0, 0);
        for unit in units.iter_mut().filter(|u| u.kept && !u.required) {
            unit.kept = false;
            tokens += unit.tokens;
            bytes += contents[unit.range.clone()]
                .iter()
                .map(|s| s.len() + 2)
                .sum::<usize>();
            output.omitted_ids.extend(
                request.entries[unit.range.clone()]
                    .iter()
                    .map(|e| e.id.clone()),
            );
            count += 1;
            if tokens >= deficit_tokens && bytes >= deficit_bytes {
                break;
            }
        }
        if count == 0 {
            break;
        }
        render(counter, &contents, &units, &marker, &mut output)?;
    }
    output.protected_overflow = !fits(request, &output);
    Ok(finish(output))
}

fn reduce(
    request: &Request,
    contents: &mut [String],
    units: &mut [Unit],
    reductions: &mut Vec<Reduction>,
) -> Result<()> {
    for unit in units {
        for i in unit.range.clone() {
            let entry = &request.entries[i];
            // Explicitly protected content is byte-exact, including user-marked code/logs.
            if entry.protected
                || matches!(
                    entry.kind,
                    Kind::Instructions | Kind::Policy | Kind::Approval | Kind::Error
                )
            {
                continue;
            }
            let mut reduction = Reduction {
                id: entry.id.clone(),
                method: "unchanged",
                before_bytes: entry.content.len(),
                after_bytes: entry.content.len(),
                diagnostic_errors: 0,
                diagnostic_warnings: 0,
                error: None,
            };
            let candidate = match entry.kind {
                Kind::Code => {
                    reduction.method = "ast_function_bodies";
                    match syntax::inspect(
                        &entry.content,
                        entry.language.context("code language missing")?,
                    )
                    .and_then(|i| i.fold(&entry.content))
                    {
                        Ok(folded) => Some(format!(
                            "[AST projection; bodies omitted; original in recovery]\n{folded}"
                        )),
                        Err(error) => {
                            reduction.error = Some(error.to_string());
                            None
                        }
                    }
                }
                Kind::Log => {
                    reduction.method = "compiler_diagnostics";
                    let log = context_diagnostics::logs(&entry.content);
                    reduction.diagnostic_errors = log.errors;
                    reduction.diagnostic_warnings = log.warnings;
                    unit.required |= log.errors > 0;
                    Some(log.text)
                }
                Kind::Diff => {
                    reduction.method = "diff_changes";
                    context_diagnostics::diff(&entry.content)
                }
                _ => None,
            };
            if let Some(text) = candidate.filter(|text| text.len() < contents[i].len()) {
                reduction.after_bytes = text.len();
                contents[i] = text;
            }
            if reduction.method != "unchanged" {
                reductions.push(reduction);
            }
        }
    }
    Ok(())
}

fn fits(request: &Request, output: &Output) -> bool {
    output.output_tokens + request.reserved_tokens <= request.max_tokens
        && output.text.len() <= request.max_bytes
}
fn render(
    counter: &Counter,
    contents: &[String],
    units: &[Unit],
    marker: &str,
    output: &mut Output,
) -> Result<()> {
    let mut parts: Vec<&str> = units
        .iter()
        .filter(|u| u.kept)
        .flat_map(|u| contents[u.range.clone()].iter().map(String::as_str))
        .collect();
    parts.push(marker);
    output.text = parts.join("\n\n");
    output.output_tokens = counter.count(&output.text)?;
    Ok(())
}
fn finish(mut output: Output) -> Output {
    output.output_bytes = output.text.len();
    output
}
