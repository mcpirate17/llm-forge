//! Native local gate: observed preflight, live closure, corpus, budget and fresh review.
#[path = "candidate_gate_budget.rs"]
mod budget;
#[path = "candidate_gate_closure.rs"]
mod closure;
#[path = "candidate_gate_corpus.rs"]
mod corpus;
#[path = "candidate_gate_ledger_inputs.rs"]
mod ledger_inputs;
#[path = "candidate_gate_output.rs"]
mod output;
#[path = "candidate_gate_programs.rs"]
mod programs;
#[path = "candidate_gate_pytest.rs"]
mod pytest;
#[path = "candidate_gate_stage.rs"]
mod stage;
#[path = "candidate_gate_tools.rs"]
mod tools;
#[path = "candidate_gate_types.rs"]
mod types;
#[path = "candidate_gate_waivers.rs"]
mod waivers;
use crate::{
    candidate_engine_identity::BuildStamp,
    candidate_execution_plan::{Selection, Surface},
    candidate_review::{self, ReviewArgs},
    candidate_snapshot::{Limits, Snapshot},
    candidate_source::Kind,
    process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::path::PathBuf;
use types::{Phase, ToolStatus};
#[derive(Args)]
pub struct GateArgs {
    #[command(flatten)]
    review: ReviewArgs,
    /// Operator-pinned native Git used for corpus patch applicability.
    #[arg(long)]
    git: PathBuf,
    #[arg(long)]
    git_sha256: String,
    /// Operator-owned declaration of this Git build's system attributes path.
    #[arg(long)]
    git_build_profile: PathBuf,
    #[arg(long)]
    git_build_profile_sha256: String,
    /// Require this granted program alias for pytest configuration probing.
    #[arg(long)]
    python: Option<String>,
    #[arg(long)]
    skip_pytest_config: bool,
    #[arg(long)]
    skip_review: bool,
    /// Newly sealed review artifact, under an explicitly writable parent.
    #[arg(long, default_value = "tasks/audit/gate.json")]
    json_out: PathBuf,
    #[arg(long)]
    json: bool,
}
fn admit(args: &GateArgs, selection: &Selection) -> Result<()> {
    ensure!(
        selection.date() == crate::candidate_plan::today()?,
        "gate date must equal current UTC date"
    );
    ensure!(
        matches!(selection.candidate.kind, Kind::Range),
        "gate requires --kind range"
    );
    ensure!(
        matches!(selection.surface, Some(Surface::Ci)),
        "gate requires --surface ci"
    );
    ensure!(
        matches!(selection.profile(), "fast" | "full"),
        "gate supports fast and full profiles"
    );
    if !args.skip_review {
        ensure!(
            selection.grants.batch.is_some(),
            "complete review requires an operator batch budget"
        );
    }
    if let Some(python) = &args.python {
        ensure!(
            selection.grants.python.as_deref() == Some(python.as_str()),
            "--python must name the granted Python alias"
        );
    }
    if let Some(git) = selection.grants.programs.get("git") {
        ensure!(
            git.canonicalize()? == args.git.canonicalize()?,
            "granted Git differs from the pinned corpus Git executable"
        );
    }
    if let Some(forge) = selection.grants.programs.get("forge") {
        use std::os::unix::fs::MetadataExt;
        let granted = std::fs::metadata(forge)?;
        let running = std::fs::metadata("/proc/self/exe")?;
        ensure!(
            (granted.dev(), granted.ino()) == (running.dev(), running.ino()),
            "granted Forge differs from the running review engine"
        );
    }
    ensure!(
        git2::Repository::discover(&selection.grants.scratch_parent).is_err(),
        "gate scratch parent must be outside Git repositories"
    );
    Ok(())
}
fn review_checks(value: &Value, expected: [&str; 5]) -> (bool, bool) {
    let binding = &value["binding"];
    let bound = [
        "candidate_tree_oid",
        "base_tree_oid",
        "policy_sha256",
        "grants_sha256",
        "profile",
    ]
    .iter()
    .zip(expected)
    .all(|(key, expected)| binding[*key].as_str() == Some(expected))
        && binding["surface"] == "ci";
    let complete = value["executed"] == true
        && value["review_complete"] == true
        && value["execution_verified"] == true
        && value["execution_scope"] == "complete_active_profile";
    (bound, complete)
}
fn review_phase(report: Result<Value>, selection: &Selection) -> Result<Phase> {
    review_result(
        report,
        [
            &selection.candidate.tree_oid,
            &selection.candidate.base_tree_oid,
            &selection.policy.digest,
            selection.grants.digest(),
            selection.profile(),
        ],
    )
}
fn review_result(report: Result<Value>, expected: [&str; 5]) -> Result<Phase> {
    let value = report.context("review did not complete")?;
    let (bound, complete) = review_checks(&value, expected);
    ensure!(bound, "review binding does not match gate selection");
    ensure!(complete, "review execution is incomplete or unverified");
    let decision = value["decision"]
        .as_str()
        .context("review decision missing")?;
    ensure!(
        matches!(decision, "pass" | "fail"),
        "unsupported review decision"
    );
    Ok(Phase::new(
        "review",
        decision == "pass",
        format!("decision={decision} complete={complete} captured_binding={bound}"),
        json!({"decision":decision,"complete":complete,"captured_binding":bound}),
    ))
}

fn pin_review(args: &mut GateArgs, selection: &Selection) -> Result<()> {
    args.review.execute = true;
    args.review.selection.target_ref = selection
        .candidate
        .commit_oid
        .clone()
        .context("range target commit missing")?;
    args.review.selection.base_ref = Some(
        selection
            .candidate
            .base_commit_oid
            .clone()
            .context("range merge base missing")?,
    );
    args.review.selection.expected_tree = Some(selection.candidate.tree_oid.clone());
    let source = selection.source()?;
    let receipt = output::receipt(source.root(), &args.json_out)?;
    args.review.set_gate_receipt(receipt)
}
fn snapshot_run(
    mut args: GateArgs,
    selection: &Selection,
    source: &crate::candidate_source::Source,
    snapshot: &Snapshot,
    progress: (&mut Vec<Phase>, &mut Vec<ToolStatus>),
    stamp: &BuildStamp,
    interrupts: &Interrupts,
) -> Result<u8> {
    let (phases, statuses) = progress;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let programs = programs::Programs::capture(selection, args.skip_pytest_config)?;
    let (tool_phase, observed) = tools::run(selection, interrupts, &runtime)?;
    let tool_ok = tool_phase.ok;
    phases.push(tool_phase);
    *statuses = observed;
    if !tool_ok {
        return Ok(2);
    }
    let pytest = pytest::run(
        selection,
        source,
        snapshot,
        interrupts,
        &runtime,
        args.skip_pytest_config,
    )?;
    let pytest_ok = pytest.phase.ok;
    phases.push(pytest.phase.clone());
    if !pytest_ok {
        return Ok(2);
    }
    phases.push(waivers::run(selection)?);
    let closure = closure::Closure::collect(source)?;
    phases.push(closure.phase.clone());
    let corpus = corpus::collect(&args, selection, source, snapshot, interrupts, stamp)?;
    phases.push(corpus.phase.clone());
    let budget = budget::collect(source, snapshot, interrupts)?;
    phases.push(budget.phase.clone());
    ensure!(!interrupts.interrupted()?, "gate interrupted before review");
    ensure!(
        selection.date() == crate::candidate_plan::today()?,
        "gate date changed before review"
    );
    if !args.skip_review {
        pin_review(&mut args, selection)?;
        let report = candidate_review::run_with_interrupts(args.review, stamp, interrupts);
        phases.push(review_phase(report, selection)?);
    }
    ensure!(!interrupts.interrupted()?, "gate interrupted");
    ensure!(
        selection.date() == crate::candidate_plan::today()?,
        "gate date changed before disposition"
    );
    let current = selection.source()?;
    closure.verify(&current)?;
    programs.verify()?;
    pytest.verify()?;
    corpus.verify()?;
    budget.verify()?;
    Ok(verdict(phases))
}
fn execute(
    mut args: GateArgs,
    phases: &mut Vec<Phase>,
    statuses: &mut Vec<ToolStatus>,
    stamp: &BuildStamp,
) -> Result<u8> {
    if args.review.selection.profile.is_none() {
        args.review.selection.profile = Some(crate::candidate_plan::Profile::Full);
    }
    if args.review.selection.base_ref.is_none() {
        args.review.selection.base_ref = Some("origin/w7-trident-program".into());
    }
    let selection = Selection::load(&args.review.selection)?;
    admit(&args, &selection)?;
    let source = selection.source()?;
    let (grants_binding, _) = crate::candidate_inputs::external(
        &args.review.selection.grants,
        selection.grants.digest(),
        1024 * 1024,
        source.root(),
    )?;
    let interrupts = Interrupts::install()?;
    let snapshot = Snapshot::create(
        &source,
        &selection.candidate.tree_oid,
        &selection
            .grants
            .scratch_parent
            .join(crate::resource_id("forge-gate")?),
        Limits::default(),
    )?;
    let result = if snapshot.report().skipped_gitlinks != 0 {
        Err(anyhow::anyhow!(
            "gate refuses candidate export that omits Git submodules"
        ))
    } else {
        phases.push(Phase::new("export", true,
            format!("exported {} candidate file(s)", snapshot.report().materialized_files),
            json!({"tree_oid":selection.candidate.tree_oid,"exported_files":snapshot.report().materialized_files})));
        snapshot_run(
            args,
            &selection,
            &source,
            &snapshot,
            (phases, statuses),
            stamp,
            &interrupts,
        )
    };
    let grant_check = grants_binding.verify();
    let cleanup = snapshot.discard();
    match (result, grant_check, cleanup) {
        (Ok(code), Ok(()), Ok(())) => {
            ensure!(
                !interrupts.interrupted()?,
                "gate interrupted during snapshot cleanup"
            );
            Ok(code)
        }
        (result, grants, cleanup) => {
            anyhow::bail!(
            "gate disposition failed: result={:?}; grant verification={:?}; snapshot cleanup={:?}",
            result.err(), grants.err(), cleanup.err()
        )
        }
    }
}
fn verdict(phases: &[Phase]) -> u8 {
    u8::from(phases.iter().any(|phase| !phase.ok))
}
fn label(code: u8) -> &'static str {
    match code {
        0 => "PASS",
        2 => "REFUSED",
        _ => "FAIL",
    }
}
pub fn run(args: GateArgs, stamp: &BuildStamp) -> u8 {
    let json_output = args.json;
    let review_skipped = args.skip_review;
    let pytest_skipped = args.skip_pytest_config;
    let mut phases = Vec::new();
    let mut statuses = Vec::new();
    let code = match execute(args, &mut phases, &mut statuses, stamp) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("gate | REFUSED\n  {error:#}");
            2
        }
    };
    if json_output {
        println!(
            "{}",
            json!({"exit_code":code,"phases":phases,"tools":statuses,"review_skipped":review_skipped,"pytest_skipped":pytest_skipped})
        );
    } else {
        println!(
            "gate | {}{}{}",
            label(code),
            if review_skipped {
                " (review skipped)"
            } else {
                ""
            },
            if pytest_skipped {
                " (pytest config skipped)"
            } else {
                ""
            }
        );
        for phase in phases {
            println!(
                "  [{}] {}: {}",
                if phase.ok { "ok " } else { "FAIL" },
                phase.name,
                phase.detail
            );
        }
    }
    code
}

#[cfg(test)]
#[path = "candidate_gate_tests.rs"]
mod tests;
