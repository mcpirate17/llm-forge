use super::*;
use crate::{
    candidate_assessment::{CheckResult, CheckStatus, Finding},
    candidate_execution_plan::Surface,
    candidate_source::Source,
    candidate_test_selection::{self, ExternalRegistry},
};

pub(super) struct Observation {
    pub results: result::Results,
    pub evidence: Value,
    pub stages: Vec<Value>,
    pub baselines: Vec<Value>,
    pub cancelled: bool,
}

fn special(id: &str) -> bool {
    matches!(
        id,
        "test-evidence"
            | "performance-evidence"
            | "research-integrity"
            | "ownership"
            | "targeted-tests"
            | "targeted-tests-full"
            | "mutation-evidence"
            | "equivalence-probe"
    )
}
fn insert_if_requested(
    results: &mut result::Results,
    plans: &[Arc<CheckPlan>],
    row: &CheckResult,
) -> Result<()> {
    if plans.iter().any(|p| p.id == row.check_id) {
        results.insert(row.clone(), true)?;
    }
    Ok(())
}
fn baselines(selection: &Selection, snapshot: &Snapshot) -> Result<Vec<Value>> {
    let classes = selection
        .candidate
        .changes
        .iter()
        .flat_map(|c| c.classes.iter().map(String::as_str))
        .collect();
    crate::candidate_plan::baseline_receipts(
        &snapshot.path(),
        &selection.policy.document,
        selection.profile(),
        &classes,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    args: &ReviewArgs,
    selection: &Arc<Selection>,
    source: &Source,
    snapshot: &Snapshot,
    plans: &[Arc<CheckPlan>],
    stamp: &BuildStamp,
    runtime: &tokio::runtime::Runtime,
    interrupts: &Interrupts,
    parallelism: usize,
) -> Result<Observation> {
    let mut observed = Observation {
        results: Default::default(),
        evidence: json!({}),
        stages: Vec::new(),
        baselines: Vec::new(),
        cancelled: false,
    };
    match baselines(selection, snapshot) {
        Ok(value) => observed.baselines = value,
        Err(error) => observed.results.obligation("baselines", error),
    }
    engine(selection, source, snapshot, stamp, &mut observed)?;
    let context = Captured {
        source,
        snapshot,
        selection,
        plans,
    };
    let tests = test_selection(&context, args, &mut observed)?;
    let evidence = source_evidence(&context, tests.as_ref(), &mut observed)?;
    let mutation = run_mutation(
        args,
        &context,
        stamp,
        selection,
        runtime,
        interrupts,
        &mut observed,
    )?;
    let equivalence = run_equivalence(
        args,
        selection,
        snapshot,
        plans,
        runtime,
        interrupts,
        parallelism,
        &mut observed,
    )?;
    let ownership = ownership(
        selection,
        source,
        snapshot,
        plans,
        &args.owner,
        &mut observed,
    )?;
    run_regular(
        selection,
        plans,
        runtime,
        interrupts,
        parallelism,
        &mut observed,
    )?;
    run_targeted(
        selection,
        plans,
        tests.as_ref(),
        runtime,
        interrupts,
        parallelism,
        &mut observed,
    )?;
    for (name, verified) in [
        ("test-selection-drift", tests.as_ref().map(|v| v.verify())),
        (
            "source-evidence-drift",
            evidence.as_ref().map(|v| v.verify()),
        ),
        (
            "mutation-evidence-drift",
            mutation.as_ref().map(|v| v.verify()),
        ),
        (
            "equivalence-input-drift",
            equivalence.as_ref().map(|v| v.verify()),
        ),
        ("ownership-drift", ownership.as_ref().map(|v| v.verify())),
    ] {
        if let Some(Err(error)) = verified {
            observed.results.obligation(name, error);
        }
    }
    finish_checks(selection, source, snapshot, &mut observed)?;
    Ok(observed)
}

fn finish_checks(
    selection: &Selection,
    source: &Source,
    snapshot: &Snapshot,
    observed: &mut Observation,
) -> Result<()> {
    match baselines(selection, snapshot) {
        Ok(value) if value == observed.baselines => (),
        Ok(_) => observed
            .results
            .obligation("baseline-drift", "captured baseline identities changed"),
        Err(error) => observed.results.obligation("baseline-drift", error),
    }
    let attestation = attestation(selection, source);
    match attestation {
        Ok(value) => {
            observed.evidence["bypass"] = json!(value.metrics);
            observed.results.insert(value, true)?;
        }
        Err(error) => observed.results.failed("attestation", error)?,
    }
    Ok(())
}

fn run_mutation<'a>(
    args: &'a ReviewArgs,
    context: &Captured<'a>,
    stamp: &'a BuildStamp,
    execution: &Arc<Selection>,
    runtime: &tokio::runtime::Runtime,
    interrupts: &Interrupts,
    observed: &mut Observation,
) -> Result<Option<crate::candidate_review_mutation::Evidence<'a>>> {
    use crate::{
        candidate_mutation_runner::RunnerBundleSpec, candidate_review_mutation as mutation,
    };
    if !context
        .plans
        .iter()
        .any(|plan| plan.id == "mutation-evidence")
    {
        return Ok(None);
    }
    let collect = || -> Result<mutation::Evidence<'a>> {
        let runner = args
            .mutation_runner_bundle
            .as_deref()
            .zip(args.mutation_runner_sha256.as_deref())
            .map(|(manifest, sha256)| RunnerBundleSpec { manifest, sha256 });
        let mut value = mutation::collect(mutation::Context {
            source: context.source,
            snapshot: context.snapshot,
            candidate: &context.selection.candidate,
            policy: &context.selection.policy,
            today: context.selection.date(),
            runner,
            native_runner_pin: args
                .native_runner_pin
                .as_deref()
                .zip(args.native_runner_pin_sha256.as_deref()),
            stamp: Some(stamp),
            scratch_exclusions: std::slice::from_ref(&context.selection.grants.scratch_parent),
            grandfather: context.selection.grants.mutation_grandfather.as_ref(),
            waiver_policy_sha256: context
                .selection
                .grants
                .mutation_waivers_policy_sha256
                .as_deref(),
        })?;
        if !value.requests().is_empty() {
            let probe = value
                .probe()
                .context("mutation invariants require a pinned native probe")?;
            let measurement = runtime.block_on(crate::candidate_mutation_external::execute(
                execution.clone(),
                value.requests(),
                probe,
                interrupts,
            ))?;
            value.apply_external(measurement)?;
        }
        value.verify()?;
        Ok(value)
    };
    match collect() {
        Ok(value) => {
            observed.evidence["mutation"] = value.evidence.clone();
            observed.results.insert(value.result.clone(), true)?;
            Ok(Some(value))
        }
        Err(error) => {
            observed
                .results
                .failed("mutation-evidence", format!("{error:#}"))?;
            Ok(None)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_equivalence<'a>(
    args: &ReviewArgs,
    selection: &'a Arc<Selection>,
    snapshot: &'a Snapshot,
    plans: &[Arc<CheckPlan>],
    runtime: &tokio::runtime::Runtime,
    interrupts: &Interrupts,
    parallelism: usize,
    observed: &mut Observation,
) -> Result<Option<crate::candidate_equivalence_inputs::Captured<'a>>> {
    let Some(plan) = plans.iter().find(|p| p.id == "equivalence-probe") else {
        return Ok(None);
    };
    match super::equivalence::collect(
        args,
        selection,
        snapshot,
        plan,
        runtime,
        interrupts,
        parallelism,
    ) {
        Ok((row, evidence, captured)) => {
            observed.evidence["equivalence"] = evidence;
            observed.results.insert(row, true)?;
            Ok(Some(captured))
        }
        Err(error) => {
            observed
                .results
                .failed("equivalence-probe", format!("{error:#}"))?;
            Ok(None)
        }
    }
}

fn engine(
    selection: &Selection,
    source: &Source,
    snapshot: &Snapshot,
    stamp: &BuildStamp,
    observed: &mut Observation,
) -> Result<()> {
    match crate::candidate_engine_identity::collect(
        source,
        snapshot,
        &selection.candidate,
        &selection.policy,
        selection.date(),
        stamp,
    ) {
        Ok((evidence, row)) => {
            observed.evidence["engine"] = serde_json::to_value(evidence)?;
            observed.results.insert(row, true)?;
        }
        Err(error) => observed.results.failed("engine-integrity", error)?,
    }
    Ok(())
}

fn run_targeted(
    selection: &Arc<Selection>,
    plans: &[Arc<CheckPlan>],
    tests: Option<&candidate_test_selection::Selection<'_>>,
    runtime: &tokio::runtime::Runtime,
    interrupts: &Interrupts,
    parallelism: usize,
    observed: &mut Observation,
) -> Result<()> {
    for plan in plans
        .iter()
        .filter(|p| matches!(p.id.as_str(), "targeted-tests" | "targeted-tests-full"))
    {
        let Some(tests) = tests else {
            observed
                .results
                .failed(&plan.id, "captured test selection unavailable")?;
            continue;
        };
        let outcome = runtime.block_on(crate::candidate_targeted_execution::run(
            selection.clone(),
            tests,
            &plan.id,
            interrupts,
            parallelism,
        ));
        match outcome {
            Ok((row, evidence)) => {
                observed.results.insert(row, true)?;
                observed.evidence["targeted"][&plan.id] = evidence;
            }
            Err(error) => observed.results.failed(&plan.id, format!("{error:#}"))?,
        }
    }
    Ok(())
}

struct Captured<'a> {
    source: &'a Source,
    snapshot: &'a Snapshot,
    selection: &'a Selection,
    plans: &'a [Arc<CheckPlan>],
}

fn test_selection<'a>(
    context: &Captured<'a>,
    args: &ReviewArgs,
    observed: &mut Observation,
) -> Result<Option<candidate_test_selection::Selection<'a>>> {
    let selection = context.selection;
    // Selection is implicit even if the profile omits the test-evidence row.
    let tests = candidate_test_selection::collect(
        context.source,
        context.snapshot,
        &selection.candidate,
        &selection.policy,
        selection.date(),
        args.registry
            .as_deref()
            .zip(args.registry_sha256.as_deref())
            .map(|(path, sha256)| ExternalRegistry { path, sha256 }),
    );
    match tests {
        Ok(value) => {
            insert_if_requested(&mut observed.results, context.plans, &value.result)?;
            if value.result.status == CheckStatus::Error {
                observed.results.obligation(
                    "test-selection",
                    "implicit selection lacks required evidence",
                );
            }
            observed.evidence["test_selection"] = value.evidence.clone();
            Ok(Some(value))
        }
        Err(error) => {
            observed.results.obligation("test-selection", error);
            Ok(None)
        }
    }
}

fn source_evidence<'a, 's>(
    context: &Captured<'a>,
    tests: Option<&'s candidate_test_selection::Selection<'a>>,
    observed: &mut Observation,
) -> Result<Option<crate::candidate_review_evidence::Evidence<'a, 's>>> {
    let Some(tests) = tests else { return Ok(None) };
    if !context
        .plans
        .iter()
        .any(|p| matches!(p.id.as_str(), "performance-evidence" | "research-integrity"))
    {
        return Ok(None);
    }
    let selection = context.selection;
    match crate::candidate_review_evidence::collect(
        context.source,
        context.snapshot,
        &selection.candidate,
        &selection.policy,
        selection.date(),
        tests,
    ) {
        Ok(value) => {
            for row in &value.results {
                insert_if_requested(&mut observed.results, context.plans, row)?;
            }
            observed.evidence["source"] = value.evidence.clone();
            Ok(Some(value))
        }
        Err(error) => {
            observed.results.obligation("source-evidence", error);
            Ok(None)
        }
    }
}

fn ownership<'a>(
    selection: &'a Selection,
    source: &'a Source,
    snapshot: &'a Snapshot,
    plans: &[Arc<CheckPlan>],
    owner: &'a str,
    observed: &mut Observation,
) -> Result<Option<crate::candidate_review_ownership::Evidence<'a>>> {
    if !plans.iter().any(|p| p.id == "ownership" && p.scheduled) {
        return Ok(None);
    }
    let context = crate::candidate_review_ownership::Context {
        source,
        snapshot,
        candidate: &selection.candidate,
        policy: &selection.policy,
        today: selection.date(),
        surface: selection.surface.context("review surface missing")?,
        owner,
    };
    match crate::candidate_review_ownership::collect(context) {
        Ok(value) => {
            observed.results.insert(value.result.clone(), true)?;
            observed.evidence["ownership"] = value.evidence.clone();
            Ok(Some(value))
        }
        Err(error) => {
            observed.results.failed("ownership", error)?;
            Ok(None)
        }
    }
}

fn run_regular(
    selection: &Arc<Selection>,
    plans: &[Arc<CheckPlan>],
    runtime: &tokio::runtime::Runtime,
    interrupts: &Interrupts,
    parallelism: usize,
    observed: &mut Observation,
) -> Result<()> {
    let regular: Vec<_> = plans.iter().filter(|p| !special(&p.id)).cloned().collect();
    match runtime.block_on(crate::candidate_batch::execute_plans(
        selection.clone(),
        &regular,
        parallelism,
        interrupts,
    )) {
        Ok((rows, cancelled)) => {
            observed.cancelled = cancelled;
            ensure!(
                rows.len() == regular.len(),
                "regular inventory/result count mismatch"
            );
            for (plan, row) in regular.iter().zip(&rows) {
                observed
                    .results
                    .regular(&plan.id, row, plan.external.is_some())?;
            }
            observed.stages = rows;
            if cancelled {
                observed
                    .results
                    .obligation("cancellation", "regular execution cancelled");
            }
        }
        Err(error) => {
            for plan in &regular {
                observed.results.failed(&plan.id, &error)?;
            }
            observed.results.obligation("regular-execution", error);
        }
    }
    // Special collectors are independent of ordinary file scheduling. Ownership
    // alone uses regular scheduling; the implicit selection has already run.
    for plan in plans.iter().filter(|p| p.id == "ownership" && !p.scheduled) {
        observed.results.insert(
            result::skipped(&plan.id, "no matching candidate files"),
            true,
        )?;
    }
    Ok(())
}

fn attestation(selection: &Selection, source: &Source) -> Result<CheckResult> {
    let started = Instant::now();
    let payload = match selection.surface.context("review surface missing")? {
        Surface::PostCommit => crate::candidate_attestation::collect_post_commit(
            source,
            &selection.candidate,
            &selection.policy,
        )?,
        Surface::Ci => crate::candidate_attestation::collect_ci(source, &selection.candidate)?,
        surface => {
            json!({"findings":[],"metrics":{"surface":surface,"status":"no-commit-attestation-required"}})
        }
    };
    let mut row = result::result("attestation", CheckStatus::Passed);
    row.findings = serde_json::from_value::<Vec<Finding>>(payload["findings"].clone())?;
    ensure!(
        row.findings.iter().all(|f| f.check_id == "attestation"),
        "attestation finding identity mismatch"
    );
    for finding in &mut row.findings {
        crate::candidate_assessment::finalize_finding(finding);
    }
    if !row.findings.is_empty() {
        row.status = CheckStatus::Failed;
    }
    row.metrics = payload["metrics"]
        .as_object()
        .context("attestation metrics missing")?
        .clone();
    row.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(row)
}
