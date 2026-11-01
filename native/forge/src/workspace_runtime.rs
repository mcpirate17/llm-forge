//! Native, evidence-backed workspace runtime verification.
#[path = "workspace_runtime_checks.rs"]
mod checks;
#[path = "workspace_runtime_eval.rs"]
mod eval;
#[path = "workspace_runtime_launchers.rs"]
mod launchers;
#[path = "workspace_runtime_models.rs"]
mod models;
#[path = "workspace_runtime_reconcile.rs"]
mod reconcile;
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct RuntimeArgs {
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Directory in which to save the receipt and opt-in probe evidence.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Evaluate an existing bounded runtime receipt without making live calls.
    #[arg(long)]
    evaluate_receipt: Option<PathBuf>,
    /// Score live checks without treating an incomplete result as a valid receipt.
    #[arg(long)]
    evaluate: bool,
    /// Suppress model and launcher calls during evaluation.
    #[arg(long)]
    offline: bool,
    /// Save an evaluation result under --output as a uniquely named receipt.
    #[arg(long)]
    write_receipt: bool,
    #[arg(long, default_value_t = 0)]
    seed: i64,
    #[arg(long)]
    graph_evidence: Option<PathBuf>,
    /// Run five launcher model calls in isolated fixtures.
    #[arg(long)]
    run_launchers: bool,
    /// Run the local embedding canary against the configured broker.
    #[arg(long)]
    run_embedding: bool,
    /// Run the local clerk canary only after a successful GPU ownership preflight.
    #[arg(long)]
    run_clerk: bool,
    /// Assert that the selected clerk server is dedicated exclusively to this probe.
    #[arg(long)]
    assert_exclusive_clerk_server: bool,
    /// Dedicated operator-owned loopback Ollama server port for clerk execution.
    #[arg(long)]
    clerk_port: Option<u16>,
    /// PID of the dedicated server listening on --clerk-port.
    #[arg(long)]
    clerk_server_pid: Option<u32>,
    /// Explicit nvidia-smi executable for the GPU ownership preflight.
    #[arg(long)]
    nvidia_smi: Option<PathBuf>,
    /// Installed native provider mode used for provider verification.
    #[arg(long, default_value = "warn", value_parser = ["warn", "enforce"])]
    provider_mode: String,
    #[arg(long)]
    provider_identity: Option<String>,
    /// Task-bound query vector for native memory and card retrieval.
    #[arg(long)]
    recall_query: Option<PathBuf>,
    #[arg(long)]
    memory_source: Option<PathBuf>,
    #[arg(long)]
    cards: Option<PathBuf>,
    #[arg(
        long,
        default_value = "workspace reliability hooks embedding graph active state"
    )]
    recall_task: String,
    #[arg(long, conflicts_with_all = ["reconcile_launcher_logs", "reconcile_clerk"])]
    reconcile_graph_evidence: Option<PathBuf>,
    #[arg(long, conflicts_with = "reconcile_clerk")]
    reconcile_launcher_logs: bool,
    #[arg(long)]
    reconcile_clerk: bool,
}

pub(super) fn cell(id: &str, status: &str, detail: &str, evidence: Value) -> Value {
    json!({"cell_id":id,"status":status,"detail":detail,"required":true,"evidence":evidence})
}
pub(super) fn dispatch(operation: &str, input: &Value) -> Result<Value> {
    conductor_native::workspace_runtime_matrix::dispatch(operation, input)
        .map_err(anyhow::Error::msg)
}
fn repo_head(root: &Path) -> Result<String> {
    let repo = git2::Repository::discover(root)?;
    let head = repo.head()?.peel_to_commit()?.id().to_string();
    Ok(head)
}

pub fn run(args: RuntimeArgs) -> Result<u8> {
    ensure!(
        !args.offline
            || (!args.run_launchers
                && !args.run_embedding
                && !args.run_clerk
                && !args.reconcile_clerk),
        "--offline cannot be combined with model or launcher calls"
    );
    ensure!(
        !args.write_receipt || args.evaluate || args.evaluate_receipt.is_some(),
        "--write-receipt requires evaluation"
    );
    if let Some(path) = args.evaluate_receipt.as_deref() {
        ensure!(
            !args.run_launchers
                && !args.run_embedding
                && !args.run_clerk
                && !args.evaluate
                && args.reconcile_graph_evidence.is_none()
                && !args.reconcile_launcher_logs
                && !args.reconcile_clerk,
            "receipt evaluation cannot be combined with live probe flags"
        );
        let mut result = eval::receipt(path);
        save_evaluation(&args, &mut result)?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(if result["is_valid"] == true { 0 } else { 2 });
    }
    let root = args.root.canonicalize().context("runtime workspace root")?;
    ensure!(root.is_dir(), "runtime root must be a directory");
    let started = std::time::Instant::now();
    let temporary = if args.evaluate && args.output.is_none() {
        let path = std::env::temp_dir().join(forge::resource_id("runtime-eval")?);
        std::fs::create_dir(&path)?;
        Some(TempOutput(path))
    } else {
        None
    };
    let requested_output = args
        .output
        .as_ref()
        .or_else(|| temporary.as_ref().map(|dir| &dir.0))
        .context("--output is required for runtime checks")?;
    let output = if requested_output.is_absolute() {
        requested_output.clone()
    } else {
        root.join(requested_output)
    };
    std::fs::create_dir_all(&output)?;
    let binary = std::env::current_exe()?.canonicalize()?;
    if let Some(code) = reconcile_mode(&args, &root, &binary, &output)? {
        return Ok(code);
    }
    ensure!(
        !args.evaluate || (!args.run_launchers && !args.run_clerk && args.graph_evidence.is_none()),
        "live scoring requires a completed receipt for graph, launcher and clerk evidence"
    );
    let cells = collect_cells(&args, &root, &binary, &output)?;
    let status = dispatch("aggregate_status", &json!(cells))?;
    if args.evaluate {
        let mut result = eval::live(&cells, status, args.seed, started.elapsed());
        save_evaluation(&args, &mut result)?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(if result["is_valid"] == true { 0 } else { 2 });
    }
    persist_receipt(&root, &output, cells, status)
}

fn reconcile_mode(
    args: &RuntimeArgs,
    root: &Path,
    binary: &Path,
    output: &Path,
) -> Result<Option<u8>> {
    if args.reconcile_graph_evidence.is_some()
        || args.reconcile_launcher_logs
        || args.reconcile_clerk
    {
        ensure!(
            !args.evaluate && !args.run_launchers && !args.run_embedding && !args.run_clerk,
            "reconciliation cannot be combined with evaluation or new probe flags"
        );
        let transaction = reconcile::Transaction::open(output)?;
        let replacements = if let Some(path) = args.reconcile_graph_evidence.as_deref() {
            vec![checks::graph(Some(path))]
        } else if args.reconcile_clerk {
            ensure!(
                args.assert_exclusive_clerk_server,
                "clerk reconciliation requires --assert-exclusive-clerk-server"
            );
            vec![models::clerk(
                root,
                args.clerk_port,
                args.clerk_server_pid,
                args.nvidia_smi.as_deref(),
                args.assert_exclusive_clerk_server,
            )]
        } else {
            vec![
                checks::providers(
                    root,
                    binary,
                    &args.provider_mode,
                    args.provider_identity.as_deref(),
                ),
                checks::hook_controls(binary, output),
                reconcile::launcher_cell(output, &transaction.receipt)?,
            ]
        };
        let receipt = transaction.replace(replacements)?;
        println!("{}", serde_json::to_string_pretty(&receipt)?);
        return Ok(Some(if receipt["status"] == "PASS" { 0 } else { 2 }));
    }
    Ok(None)
}

fn collect_cells(
    args: &RuntimeArgs,
    root: &Path,
    binary: &Path,
    output: &Path,
) -> Result<Vec<Value>> {
    let mut cells = vec![
        checks::active_state(root),
        checks::providers(
            root,
            binary,
            &args.provider_mode,
            args.provider_identity.as_deref(),
        ),
        checks::hook_controls(binary, output),
        checks::launcher_versions(),
        if args.run_embedding {
            models::embedding()
        } else {
            cell(
                "embedding-canary",
                "NOT_READY",
                "embedding call not requested",
                json!({}),
            )
        },
        checks::retrievers(
            args.recall_query.as_deref(),
            args.memory_source.as_deref(),
            args.cards.as_deref(),
            &args.recall_task,
        ),
        checks::graph(args.graph_evidence.as_deref()),
    ];
    cells.push(if args.run_launchers {
        let evidence_dir = output.join(forge::resource_id("launcher-evidence")?);
        std::fs::create_dir(&evidence_dir)?;
        launchers::run(binary, &evidence_dir)
    } else {
        cell(
            "launcher-real-smokes",
            "NOT_READY",
            "launcher calls not requested",
            json!({}),
        )
    });
    cells.push(if args.run_clerk {
        models::clerk(
            root,
            args.clerk_port,
            args.clerk_server_pid,
            args.nvidia_smi.as_deref(),
            args.assert_exclusive_clerk_server,
        )
    } else {
        cell(
            "local-clerk-canary",
            "NOT_READY",
            "clerk call not requested",
            json!({}),
        )
    });
    Ok(cells)
}

fn persist_receipt(root: &Path, output: &Path, cells: Vec<Value>, status: Value) -> Result<u8> {
    let (head, head_error) = match repo_head(root) {
        Ok(head) => (Some(head), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let (digest, claim_error) = match forge::ownership::load_claims(root) {
        Ok((_, digest)) => (Some(digest), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let receipt = json!({"schema_version":1,"generated_at":forge::instant::isoformat_utc(forge::instant::now()),
        "status":status,"cells":cells,"provenance":{"repo":root,"git_head":head,
        "claim_store_sha256":digest,"git_head_error":head_error,"claim_store_error":claim_error,
        "launcher_call_budget":5,"seconds_per_launcher_call":900,
        "reported_token_budget":250000,"observation":"runtime checks are evidence, not authorization"}});
    let bytes = serde_json::to_vec_pretty(&receipt)?;
    ensure!(bytes.len() <= 1024 * 1024, "runtime receipt exceeds 1 MiB");
    let _guard = reconcile::lock(output)?;
    forge::state_publish::publish(&output.join("receipt.json"), &bytes)?;
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(if status == "PASS" { 0 } else { 2 })
}

fn save_evaluation(args: &RuntimeArgs, result: &mut Value) -> Result<()> {
    if !args.write_receipt {
        return Ok(());
    }
    let output = args
        .output
        .as_ref()
        .context("--write-receipt requires --output")?;
    let root = args.root.canonicalize()?;
    let directory = if output.is_absolute() {
        output.clone()
    } else {
        root.join(output)
    };
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(forge::resource_id("workspace-evaluation")? + ".json");
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(&serde_json::to_vec_pretty(result)?)?;
    file.sync_all()?;
    std::fs::File::open(&directory)?.sync_all()?;
    result["metrics"]["receipt"] = json!(path);
    Ok(())
}

struct TempOutput(PathBuf);
impl Drop for TempOutput {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
