//! Branch reports and atomic claim-binding metadata. Never mutates Git refs.
#[path = "branch_policy_exposure.rs"]
mod exposure;
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use conductor_native::branch_policy_runtime as core;
use serde_json::{json, Value};
use std::path::PathBuf;
#[derive(Args)]
pub struct BranchArgs {
    #[arg(long, default_value = ".", global = true)]
    repo: PathBuf,
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, hide = true, global = true)]
    live_ref: Option<String>,
    #[arg(long, hide = true, global = true, default_value_t = 24.0)]
    stale_hours: f64,
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    CheckBranch {
        name: String,
    },
    Bind {
        #[arg(long)]
        branch: String,
        #[arg(long)]
        claim: String,
        #[arg(long)]
        owner: Option<String>,
    },
    Unbind {
        #[arg(long)]
        branch: String,
    },
    Audit,
    Status,
    Exposed,
    #[command(hide = true)]
    ExposureData {
        operation: String,
    },
    #[command(hide = true)]
    RenderExposure,
}
fn output(value: &Value, json: bool, text: String, code: u8) -> Result<u8> {
    if json {
        println!("{}", serde_json::to_string(value)?);
    } else {
        println!("{text}");
    }
    Ok(code)
}
fn check_name(name: &str, moment: &str, json: bool) -> Result<u8> {
    match core::validate_name(name, moment) {
        Ok(mut row) => {
            row["ok"] = json!(true);
            row["branch"] = json!(name);
            output(
                &row,
                json,
                format!(
                    "OK: {name} -> agent={} topic={} date={}",
                    row["agent"].as_str().unwrap(),
                    row["topic"].as_str().unwrap(),
                    row["date"].as_str().unwrap()
                ),
                0,
            )
        }
        Err(error) => output(
            &json!({"ok":false,"branch":name,"reason":error.to_string()}),
            json,
            format!("REFUSED: {error}"),
            1,
        ),
    }
}
fn bind(
    root: &std::path::Path,
    branch: &str,
    claim: &str,
    owner: Option<&str>,
    moment: &str,
    json: bool,
) -> Result<u8> {
    let parsed = core::validate_name(branch, moment)?;
    match core::bind(
        root,
        branch,
        claim,
        owner.unwrap_or(parsed["agent"].as_str().unwrap()),
        moment,
    ) {
        Ok(mut row) => {
            row["ok"] = json!(true);
            output(
                &row,
                json,
                format!(
                    "BOUND: {branch} -> {claim} (owner={})",
                    row["owner"].as_str().unwrap()
                ),
                0,
            )
        }
        Err(error) => output(
            &json!({"ok":false,"reason":error.to_string()}),
            json,
            format!("REFUSED: {error}"),
            1,
        ),
    }
}
fn status_text(report: &Value) -> Result<String> {
    let rows = report["bindings"]
        .as_array()
        .context("invalid native status")?;
    let mut lines = Vec::new();
    for row in rows {
        let pr = row["pr_number"]
            .as_i64()
            .map(|number| number.to_string())
            .unwrap_or("-".into());
        lines.push(format!(
            "  {}  claim={}  owner={}  age={:.1}h  pr={}{}",
            row["branch"].as_str().unwrap(),
            row["claim_id"].as_str().unwrap(),
            row["owner"].as_str().unwrap(),
            row["age_hours"].as_f64().unwrap(),
            pr,
            if row["stale"] == true { " STALE" } else { "" }
        ));
    }
    Ok(if lines.is_empty() {
        "no branch bindings".into()
    } else {
        lines.join("\n")
    })
}
pub fn run(args: BranchArgs) -> Result<u8> {
    let root = args
        .repo
        .canonicalize()
        .context("resolve branch-policy host")?;
    anyhow::ensure!(
        root.to_str().is_some(),
        "branch-policy host path must be UTF-8"
    );
    let moment = core::now()?;
    match args.action {
        Action::CheckBranch { name } => check_name(&name, &moment, args.json),
        Action::Bind {
            branch,
            claim,
            owner,
        } => bind(&root, &branch, &claim, owner.as_deref(), &moment, args.json),
        Action::Unbind { branch } => {
            let removed = core::unbind(&root, &branch)?;
            output(
                &json!({"removed":removed,"branch":branch}),
                args.json,
                format!(
                    "{}: {branch}",
                    if removed { "UNBOUND" } else { "NO BINDING" }
                ),
                u8::from(!removed),
            )
        }
        Action::Audit => {
            let findings = core::audit(&root, &moment)?;
            let text = if findings.is_empty() {
                "OK: no branch-policy violations".into()
            } else {
                format!(
                    "VIOLATIONS: {}\n  - {}",
                    findings.len(),
                    findings.join("\n  - ")
                )
            };
            output(
                &json!({"ok":findings.is_empty(),"findings":findings}),
                args.json,
                text,
                u8::from(!findings.is_empty()),
            )
        }
        Action::Status => {
            let report = core::status(&root, &moment)?;
            let text = status_text(&report)?;
            output(&report, args.json, text, 0)
        }
        Action::Exposed => {
            let report = exposure::report(&root)?;
            let text = exposure::render(&report)?;
            output(&report, args.json, text, 0)
        }
        Action::ExposureData { operation } => {
            let value = exposure::data(
                &root,
                &operation,
                args.live_ref.as_deref(),
                args.stale_hours,
            )?;
            println!("{}", serde_json::to_string(&value)?);
            Ok(0)
        }
        Action::RenderExposure => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::io::stdin()
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            anyhow::ensure!(
                bytes.len() <= 1024 * 1024,
                "exposure render input exceeds1MiB"
            );
            let report: Value = serde_json::from_slice(&bytes)?;
            println!("{}", serde_json::to_string(&exposure::render(&report)?)?);
            Ok(0)
        }
    }
}
