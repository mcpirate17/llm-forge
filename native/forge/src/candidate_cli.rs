//! Candidate object inspection and bounded snapshots; no review approval is issued.
use crate::{
    candidate_receipt, candidate_snapshot,
    candidate_source::{Kind, Source},
};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Args)]
pub struct CandidateArgs {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Prepare or explicitly measure fresh native review workloads.
    Benchmark(Box<crate::candidate_benchmark_cli::BenchmarkArgs>),
    #[command(hide = true)]
    BenchmarkWorker(Box<crate::candidate_benchmark_cli::WorkerArgs>),
    /// Audit the captured mutation corpus against its reproducibility baseline.
    MutationCorpus(Box<crate::mutation_corpus_cli::AuditArgs>),
    /// Snapshot admitted work and hold the exclusive lease through native Git commit.
    Commit(crate::candidate_commit::CommitArgs),
    /// Review the complete active policy using captured evidence and isolated native execution.
    Review(Box<crate::candidate_review::ReviewArgs>),
    /// Run a fresh pre-commit review and bind a commit message to its result.
    AttestMessage(Box<crate::candidate_review::AttestArgs>),
    /// Pin installed compatibility runner sources; never executes or approves them.
    RunnerBundle(crate::candidate_runner_bundle_cli::BundleArgs),
    /// Validate captured configuration and inspect Python source with native analyzers.
    Analyze(crate::candidate_analysis_cli::AnalysisArgs),
    /// Compare captured crate production inputs, versions and installed metadata.
    CrateVersion(crate::candidate_crate_cli::CrateArgs),
    /// Plan or run native source checks against exact candidate changed lines.
    Scan(crate::candidate_scan_cli::ScanArgs),
    /// Check exact changed lines using an explicitly provisioned native formatter.
    ClangFormat(crate::candidate_clang_format_cli::FormatArgs),
    /// Analyze captured C/C++ build configurations and exact changed lines.
    ClangTidy(crate::candidate_clang_tidy_cli::TidyArgs),
    /// Plan or run Cargo lint tools; isolated execution belongs to the parent stage.
    CargoLint(crate::candidate_cargo_lint_cli::LintArgs),
    CargoTest(crate::candidate_cargo_test_cli::CargoTestArgs),
    PythonTest(crate::candidate_python_test_cli::PythonTestArgs),
    ContractTest(crate::candidate_contract_cli::ContractArgs),
    /// Measure one external invariant; the owning Stage requires explicit read-only /proc.
    MutationInvariant(crate::candidate_mutation_external_cli::Args),
    /// Prepare or execute one isolated external check using operator grants.
    Execute(crate::candidate_execute::ExecuteArgs),
    /// Prepare or execute ordered regular commands within an operator batch budget.
    ExecuteBatch(crate::candidate_batch::BatchArgs),
    /// Assess supplied findings against the candidate policy; does not verify execution.
    Assess(crate::candidate_assess_cli::AssessArgs),
    /// Export a digest-checked receipt; recorded decisions are not execution proof.
    Report(crate::candidate_report_cli::ReportArgs),
    /// Classify an exact candidate and plan ordered policy checks without executing them.
    Plan(crate::candidate_plan::PlanArgs),
    /// Resolve staged/committed objects and unclassified changes, without executing checks.
    Inspect {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, value_enum)]
        kind: Kind,
        #[arg(long, default_value = "HEAD")]
        target_ref: String,
        #[arg(long)]
        base_ref: Option<String>,
    },
    /// Create a private standalone snapshot from a full tree OID; never overwrites.
    Materialize(MaterializeArgs),
    /// Verify receipt digest self-consistency, optionally binding an exact commit tree.
    VerifyReceipt {
        file: PathBuf,
        #[arg(long, requires = "reference")]
        repo: Option<PathBuf>,
        #[arg(long = "ref", requires = "repo")]
        reference: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
struct MaterializeArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long)]
    tree: String,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    dry_run: bool,
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    max_blob_bytes: u64,
    #[arg(long, default_value_t = 2 * 1024 * 1024 * 1024)]
    max_tree_bytes: u64,
}

fn materialize(args: MaterializeArgs) -> Result<()> {
    let source = Source::open(&args.repo)?;
    let limits = candidate_snapshot::Limits {
        max_blob_bytes: args.max_blob_bytes,
        max_tree_bytes: args.max_tree_bytes,
    };
    let report = if args.dry_run {
        candidate_snapshot::preflight(&source, &args.tree, &args.output, limits)?
    } else {
        candidate_snapshot::Snapshot::create(&source, &args.tree, &args.output, limits)?
            .publish()?
    };
    print_json(&serde_json::to_value(report)?)
}

fn print_json(value: &Value) -> Result<()> {
    let output = serde_json::to_string(value)?;
    ensure!(
        output.len() <= 128 * 1024 * 1024,
        "candidate JSON output exceeds 128 MiB"
    );
    println!("{output}");
    Ok(())
}

pub fn run(args: CandidateArgs) -> Result<u8> {
    run_with_stamp(args, None)
}

pub fn run_with_stamp(
    args: CandidateArgs,
    stamp: Option<&crate::candidate_engine_identity::BuildStamp>,
) -> Result<u8> {
    match args.action {
        Action::Benchmark(args) => return crate::candidate_benchmark_cli::run(*args),
        Action::BenchmarkWorker(args) => {
            return crate::candidate_benchmark_cli::worker(
                *args,
                stamp.context("benchmark worker requires the CLI's compiled engine identity")?,
            )
        }
        Action::MutationCorpus(args) => return crate::mutation_corpus_cli::run(*args),
        Action::Commit(args) => return crate::candidate_commit::run(args),
        Action::Review(args) => {
            let report = crate::candidate_review::run(
                *args,
                stamp.context("review requires the CLI's compiled engine identity")?,
            )?;
            let failed = report["executed"] == true && report["decision"] != "pass";
            print_json(&report)?;
            return Ok(u8::from(failed));
        }
        Action::AttestMessage(args) => {
            let report = crate::candidate_review::attest(
                *args,
                stamp.context("attestation requires the CLI's compiled engine identity")?,
            )?;
            print_json(&report)?;
            return Ok(0);
        }
        Action::RunnerBundle(args) => crate::candidate_runner_bundle_cli::run(args)?,
        Action::Analyze(args) => return crate::candidate_analysis_cli::run(args),
        Action::CrateVersion(args) => return crate::candidate_crate_cli::run(args),
        Action::Scan(args) => return crate::candidate_scan_cli::run(args),
        Action::ClangFormat(args) => return crate::candidate_clang_format_cli::run(args),
        Action::ClangTidy(args) => return crate::candidate_clang_tidy_cli::run(args),
        Action::CargoLint(args) => return crate::candidate_cargo_lint_cli::run(args),
        Action::CargoTest(args) => return crate::candidate_cargo_test_cli::run(args),
        Action::PythonTest(args) => return crate::candidate_python_test_cli::run(args),
        Action::ContractTest(args) => return crate::candidate_contract_cli::run(args),
        Action::MutationInvariant(args) => {
            return crate::candidate_mutation_external_cli::run(args)
        }
        Action::Report(args) => crate::candidate_report_cli::run(args)?,
        Action::ExecuteBatch(args) => {
            let report = crate::candidate_batch::run(args)?;
            let failed = report["success"] != true;
            print_json(&report)?;
            return Ok(u8::from(failed));
        }
        Action::Execute(args) => {
            let report = crate::candidate_execute::run(args)?;
            let failed = report["result"]["status"] == "failed";
            print_json(&report)?;
            return Ok(u8::from(failed));
        }
        Action::Assess(args) => {
            let report = crate::candidate_assess_cli::run(args)?;
            let failed = report["assessment"]["decision"] == "fail";
            print_json(&report)?;
            return Ok(u8::from(failed));
        }
        Action::Plan(args) => print_json(&crate::candidate_plan::run(args)?)?,
        Action::Inspect {
            repo,
            kind,
            target_ref,
            base_ref,
        } => {
            let candidate = Source::open(&repo)?.resolve(kind, &target_ref, base_ref.as_deref())?;
            print_json(&serde_json::to_value(candidate)?)?;
        }
        Action::Materialize(args) => materialize(args)?,
        Action::VerifyReceipt {
            file,
            repo,
            reference,
            json,
        } => verify(file, repo, reference, json)?,
    }
    Ok(0)
}

fn verify(
    file: PathBuf,
    repo: Option<PathBuf>,
    reference: Option<String>,
    as_json: bool,
) -> Result<()> {
    let receipt = candidate_receipt::parse_and_verify(&crate::bounded_file::read(
        &file,
        candidate_receipt::MAX_BYTES,
    )?)?;
    let binding = match (repo, reference) {
        (Some(repo), Some(reference)) => {
            let source = Source::open(&repo)?;
            let commit = source.resolve_commit(&reference)?;
            let tree = source.commit_tree(&commit)?;
            let recorded = receipt
                .payload()
                .get("candidate")
                .and_then(|value| value.get("tree_oid"))
                .and_then(Value::as_str)
                .context("candidate receipt tree OID is missing")?;
            ensure!(recorded == tree, "candidate receipt tree binding mismatch");
            Some(json!({"commit_oid":commit,"tree_oid":tree}))
        }
        (None, None) => None,
        _ => anyhow::bail!("receipt tree binding requires both --repo and --ref"),
    };
    if as_json {
        print_json(
            &json!({"digest_valid":true,"receipt_id":receipt.receipt_id(),
            "receipt_digest":receipt.digest(),"decision":receipt.decision(),"tree_binding":binding}),
        )?;
    } else {
        // JSON-escape display fields so a receipt cannot inject terminal control sequences.
        println!(
            "digest valid; receipt {}; decision {}; tree binding {}",
            serde_json::to_string(receipt.receipt_id())?,
            serde_json::to_string(receipt.decision())?,
            if binding.is_some() {
                "verified"
            } else {
                "not requested"
            }
        );
    }
    Ok(())
}
