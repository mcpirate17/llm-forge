//! Run one policy command with separately supplied operator capabilities.
//! Results describe an attempt, not review approval or analyzer completeness.
#[path = "candidate_execute_stage.rs"]
mod stage;
use crate::{
    candidate_command::{ExternalCheck, OperatorInputs},
    candidate_execution_result::{self, VersionEvidence},
    candidate_execution_workspace::{Grants, Workspace},
    candidate_plan::Profile,
    candidate_policy::{self, Policy},
    candidate_source::{Candidate, Kind, Source},
    confined_execution::Limits,
    process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};

#[derive(Args)]
pub struct ExecuteArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long, value_enum)]
    kind: Kind,
    #[arg(long, default_value = "HEAD")]
    target_ref: String,
    #[arg(long)]
    base_ref: Option<String>,
    #[arg(long)]
    policy: Option<String>,
    #[arg(long, value_enum, default_value = "fast")]
    profile: Profile,
    #[arg(long)]
    today: Option<String>,
    /// Exact active external check ID; does not execute a whole review pipeline.
    #[arg(long)]
    check: String,
    /// Owned version-1 operator grants JSON outside the candidate repository.
    #[arg(long)]
    grants: PathBuf,
    /// Refuse preparation unless the resolved candidate has this exact tree OID.
    #[arg(long)]
    expected_tree: Option<String>,
    /// Launch version and command stages; otherwise only prepare and validate.
    #[arg(long)]
    execute: bool,
}

struct Prepared {
    source: Source,
    candidate: Candidate,
    policy: Policy,
    grants: Grants,
    normalized: Value,
    check: ExternalCheck,
    files: Vec<String>,
    date: String,
    profile: &'static str,
    limits: Limits,
}

fn limits(check: &Value, grants: &Grants) -> Result<Limits> {
    let cpu = check["timeout_seconds"]
        .as_u64()
        .context("missing CPU timeout")?;
    let memory = check["memory_mb"]
        .as_u64()
        .context("missing address-space limit")?;
    let wall = check["wall_timeout_override"]
        .as_u64()
        .context("missing wall timeout")?;
    let wall = if wall == 0 { cpu } else { wall };
    ensure!(
        cpu > 0 && cpu <= grants.max_cpu_seconds,
        "check CPU timeout exceeds operator grant"
    );
    ensure!(
        wall > 0 && wall <= grants.max_wall_seconds,
        "check wall timeout exceeds operator grant"
    );
    ensure!(
        memory > 0 && memory <= grants.max_address_space_mib,
        "check address-space limit exceeds operator grant"
    );
    Ok(Limits {
        timeout: Duration::from_secs(wall),
        max_output_chars: usize::try_from(
            check["max_output_chars"]
                .as_u64()
                .context("missing output limit")?,
        )?,
        cpu_seconds: Some(cpu),
        address_space_bytes: Some(
            memory
                .checked_mul(1024 * 1024)
                .context("address-space limit overflow")?,
        ),
    })
}

fn prepare(args: &ExecuteArgs) -> Result<Prepared> {
    let date = args
        .today
        .clone()
        .map(Ok)
        .unwrap_or_else(crate::candidate_plan::today)?;
    candidate_policy::validate_today(&date)?;
    let source = Source::open(&args.repo)?;
    let mut candidate = source.resolve(args.kind, &args.target_ref, args.base_ref.as_deref())?;
    if let Some(expected) = &args.expected_tree {
        ensure!(
            *expected == candidate.tree_oid,
            "candidate tree differs from --expected-tree"
        );
    }
    let grants = Grants::load(&args.grants, &source)?;
    let workspace = Workspace::new(&source, &candidate.tree_oid, &grants)?;
    let policy = candidate_policy::load(&workspace.path(), args.policy.as_deref(), &date)?;
    policy.classify_candidate(&mut candidate)?;
    let normalized = policy
        .active_checks(args.profile.name())?
        .into_iter()
        .find(|check| check["check_id"].as_str() == Some(args.check.as_str()))
        .context("requested check is absent or inactive in this profile")?;
    let check = ExternalCheck::from_normalized(&normalized)?
        .context("execute requires an external command check")?;
    let changes = serde_json::to_value(&candidate.changes)?;
    let files = conductor_native::candidate_checks::select_files(
        changes.as_array().context("invalid candidate changes")?,
        &normalized,
    )
    .map_err(anyhow::Error::msg)?
    .into_iter()
    .map(str::to_owned)
    .collect();
    let limits = limits(&normalized, &grants)?;
    workspace.discard()?;
    Ok(Prepared {
        source,
        candidate,
        policy,
        grants,
        normalized,
        check,
        files,
        date,
        profile: args.profile.name(),
        limits,
    })
}

impl Prepared {
    fn version_limits(&self) -> Limits {
        Limits {
            timeout: Duration::from_secs(
                self.limits
                    .cpu_seconds
                    .expect("validated CPU limit")
                    .min(30)
                    .min(self.grants.max_wall_seconds),
            ),
            max_output_chars: self.limits.max_output_chars,
            cpu_seconds: self.limits.cpu_seconds.map(|seconds| seconds.min(30)),
            address_space_bytes: self.limits.address_space_bytes,
        }
    }
    fn output(&self, executed: bool) -> Value {
        json!({"schema_version":1,"executed":executed,"skipped":false,"today":self.date,"profile":self.profile,
            "candidate":self.candidate,"policy":{"path":self.policy.path,"sha256":self.policy.digest},
            "grants_sha256":self.grants.digest(),"check":self.normalized,"files":self.files,"stages":[],"result":null})
    }
    fn stage(&self, version: bool) -> Result<stage::Stage> {
        let workspace = Workspace::new(&self.source, &self.candidate.tree_oid, &self.grants)?;
        let path = workspace.path();
        let runtime = workspace.runtime();
        let prepared = crate::candidate_command::prepare(
            &self.check,
            &self.candidate,
            &OperatorInputs {
                repo: self.source.root(),
                snapshot: &path,
                runtime_dir: &runtime,
                python: self.grants.python.as_deref(),
            },
            &self.files,
        )?;
        let argv = if version {
            prepared.version
        } else {
            prepared.command
        };
        let plan = workspace.plan(&argv, &self.grants)?;
        let limits = if version {
            self.version_limits()
        } else {
            self.limits
        };
        let data = json!({"stage":if version {"version"}else{"command"},"command":argv.argv,
            "program":self.grants.programs.get(&argv.argv[0]),"snapshot":path,
            "sandbox":workspace.sandbox_report()?,"resources":self.grants.resources,
            "limits":{"timeout_seconds":limits.timeout.as_secs(),"max_output_chars":limits.max_output_chars,
                "cpu_seconds":limits.cpu_seconds,"address_space_bytes":limits.address_space_bytes},"report":null});
        Ok(stage::Stage {
            workspace,
            plan,
            limits,
            argv: argv.argv,
            data,
        })
    }
}

pub fn run(args: ExecuteArgs) -> Result<Value> {
    // Install before the runtime creates workers; children restore the prior mask.
    let interrupts = if args.execute {
        Some(Interrupts::install()?)
    } else {
        None
    };
    let prepared = prepare(&args)?;
    let mut output = prepared.output(args.execute);
    if !prepared.normalized["always"]
        .as_bool()
        .context("missing always flag")?
        && prepared.files.is_empty()
    {
        output["skipped"] = json!(true);
        output["skipped_reason"] = json!("no selected candidate files");
        return Ok(output);
    }
    if !args.execute {
        let version = prepared.stage(true)?.dry_run()?;
        let command = prepared.stage(false)?.dry_run()?;
        output["stages"] = json!([version, command]);
        return Ok(output);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()?;
    runtime.block_on(execute(
        &prepared,
        interrupts.as_ref().context("interrupt owner missing")?,
        &mut output,
    ))?;
    Ok(output)
}

async fn execute(prepared: &Prepared, interrupts: &Interrupts, output: &mut Value) -> Result<()> {
    let (version_data, version_report, version_argv) = prepared
        .stage(true)?
        .execute(&prepared.grants, interrupts)
        .await?;
    let evidence = candidate_execution_result::version(
        &version_report,
        prepared.version_limits().max_output_chars,
    )?;
    let (stages, report, command) = if matches!(evidence, VersionEvidence::Available(_)) {
        let (command_data, report, argv) = prepared
            .stage(false)?
            .execute(&prepared.grants, interrupts)
            .await?;
        (vec![version_data, command_data], report, argv)
    } else {
        // No analyzer attempt occurred. The observed failed version invocation is
        // retained as command evidence; stages explicitly identify its role.
        (vec![version_data], version_report, version_argv)
    };
    let result = candidate_execution_result::map_result(
        &prepared.normalized,
        &prepared.files,
        &command,
        Some(&evidence),
        &report,
    )?;
    output["stages"] = json!(stages);
    output["result"] = serde_json::to_value(result)?;
    Ok(())
}
