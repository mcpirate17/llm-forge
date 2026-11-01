//! Shared immutable candidate selection and per-stage native preparation.
#[path = "candidate_execute_stage.rs"]
pub(crate) mod stage;
use crate::{
    candidate_command::{ExternalCheck, OperatorInputs},
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
pub(crate) use stage::Stage;
use std::{path::PathBuf, time::Duration};

#[derive(Clone, Copy, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Surface {
    PreCommit,
    PostCommit,
    Ci,
    Manual,
}

#[derive(Args)]
pub(crate) struct SelectionArgs {
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
    /// Owned version-1 operator grants JSON outside the repository.
    #[arg(long)]
    grants: PathBuf,
    #[arg(long)]
    expected_tree: Option<String>,
    /// Explicit review surface; required by scheduled candidate-integrity checks.
    #[arg(long, value_enum)]
    surface: Option<Surface>,
}

pub(crate) struct Selection {
    root: PathBuf,
    common: PathBuf,
    pub candidate: Candidate,
    pub policy: Policy,
    pub grants: Grants,
    pub surface: Option<Surface>,
    changes: Vec<Value>,
    date: String,
    profile: &'static str,
}
pub(crate) struct CheckPlan {
    pub id: String,
    pub normalized: Value,
    pub files: Vec<String>,
    pub scheduled: bool,
    pub external: Option<ExternalCheck>,
    limits: Option<Limits>,
}

impl Selection {
    pub(crate) fn load(args: &SelectionArgs) -> Result<Self> {
        let date = args
            .today
            .clone()
            .map(Ok)
            .unwrap_or_else(crate::candidate_plan::today)?;
        candidate_policy::validate_today(&date)?;
        let source = Source::open(&args.repo)?;
        let mut candidate =
            source.resolve(args.kind, &args.target_ref, args.base_ref.as_deref())?;
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
        let Value::Array(changes) = serde_json::to_value(&candidate.changes)? else {
            anyhow::bail!("candidate changes must be an array");
        };
        workspace.discard()?;
        Ok(Self {
            root: source.root().into(),
            common: source.common_dir().into(),
            candidate,
            policy,
            grants,
            surface: args.surface,
            changes,
            date,
            profile: args.profile.name(),
        })
    }
    pub(crate) fn checks(&self) -> Result<Vec<Value>> {
        self.policy.active_checks(self.profile)
    }
    pub(crate) fn output(&self) -> Value {
        json!({"schema_version":1,"today":self.date,"profile":self.profile,
            "candidate":self.candidate,"policy":{"path":self.policy.path,"sha256":self.policy.digest},
            "grants_sha256":self.grants.digest(),"surface":self.surface})
    }
    pub(crate) fn source(&self) -> Result<Source> {
        let source = Source::open(&self.root)?;
        ensure!(
            source.root() == self.root && source.common_dir() == self.common,
            "candidate source identity changed after selection"
        );
        Ok(source)
    }
    pub(crate) fn plan(&self, normalized: Value) -> Result<CheckPlan> {
        let id = normalized["check_id"]
            .as_str()
            .context("missing check ID")?
            .to_owned();
        let external = ExternalCheck::from_normalized(&normalized)?;
        let limits = if external.is_some() {
            Some(limits(&normalized, &self.grants)?)
        } else {
            None
        };
        let selected = conductor_native::candidate_checks::select_files(&self.changes, &normalized)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            selected
                .iter()
                .try_fold(0usize, |n, path| n.checked_add(path.len()))
                .is_some_and(|bytes| bytes <= 8 * 1024 * 1024),
            "selected check files exceed 8 MiB"
        );
        let files: Vec<String> = selected.into_iter().map(str::to_owned).collect();
        let scheduled = normalized["always"]
            .as_bool()
            .context("missing always flag")?
            || !files.is_empty();
        Ok(CheckPlan {
            id,
            normalized,
            files,
            scheduled,
            external,
            limits,
        })
    }
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
                .context("address-space overflow")?,
        ),
    })
}
impl CheckPlan {
    pub(crate) fn supported(&self) -> bool {
        self.external.is_some() || crate::candidate_builtin::supports(self)
    }
    pub(crate) fn version_limits(&self, selection: &Selection) -> Result<Limits> {
        let limits = self.limits.context("command limits missing")?;
        let cpu = limits
            .cpu_seconds
            .context("command CPU limit missing")?
            .min(30);
        Ok(Limits {
            timeout: Duration::from_secs(cpu.min(selection.grants.max_wall_seconds)),
            cpu_seconds: Some(cpu),
            ..limits
        })
    }
    pub(crate) fn output(&self, executed: bool) -> Value {
        json!({"executed":executed,"check":self.normalized,"files":self.files,
            "skipped":!self.scheduled,"stages":[],"result":null})
    }
    pub(crate) fn stage(&self, selection: &Selection, version: bool) -> Result<Stage> {
        let check = self
            .external
            .as_ref()
            .context("stage requires external command")?;
        let source = selection.source()?;
        let workspace = Workspace::new(&source, &selection.candidate.tree_oid, &selection.grants)?;
        let path = workspace.path();
        let runtime = workspace.runtime();
        let prepared = crate::candidate_command::prepare(
            check,
            &selection.candidate,
            &OperatorInputs {
                repo: &selection.root,
                snapshot: &path,
                runtime_dir: &runtime,
                python: selection.grants.python.as_deref(),
            },
            &self.files,
        )?;
        let argv = if version {
            prepared.version
        } else {
            prepared.command
        };
        let plan = workspace.plan(&argv, &selection.grants)?;
        let limits = if version {
            self.version_limits(selection)?
        } else {
            self.limits.context("command limits missing")?
        };
        let data = json!({"stage":if version {"version"}else{"command"},"command":argv.argv,
            "program":selection.grants.programs.get(&argv.argv[0]),"snapshot":path,
            "sandbox":workspace.sandbox_report()?,"resources":selection.grants.resources,
            "limits":{"timeout_seconds":limits.timeout.as_secs(),"max_output_chars":limits.max_output_chars,
                "cpu_seconds":limits.cpu_seconds,"address_space_bytes":limits.address_space_bytes},"report":null});
        Ok(Stage {
            workspace,
            plan,
            limits,
            argv: argv.argv,
            data,
        })
    }
}
