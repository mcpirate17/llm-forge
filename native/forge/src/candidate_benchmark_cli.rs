//! Explicit preparation and real native review measurements. No implicit guest runs.
#[path = "candidate_benchmark_fixture.rs"]
mod fixture;
#[path = "candidate_benchmark_measure.rs"]
mod measure;
use crate::{
    candidate_engine_identity::BuildStamp, candidate_execution_workspace::Grants,
    candidate_source::Source,
};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Args)]
pub struct BenchmarkArgs {
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Prepare captured HEAD and exact scenario trees; never launches a reviewer.
    Prepare(PrepareArgs),
    /// Plan cold/warm measurements; requires --execute and fixture-bound grants to run.
    Run(Box<RunArgs>),
}
#[derive(Args)]
struct PrepareArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Captured immutable Git input; index supports staged prospective implementation.
    #[arg(long, value_enum, default_value = "commit")]
    kind: crate::candidate_source::Kind,
    /// New standalone benchmark data directory under an existing parent.
    #[arg(long)]
    fixture: PathBuf,
    /// Repeat to select scenarios; default retains the six historical scenario names.
    #[arg(long)]
    scenario: Vec<String>,
}
#[derive(Args)]
struct RunArgs {
    #[arg(long)]
    fixture: PathBuf,
    #[arg(long)]
    execute: bool,
    #[arg(long)]
    grants: Option<PathBuf>,
    /// Expected prepared-manifest SHA256, required for execution.
    #[arg(long)]
    fixture_sha256: Option<String>,
    /// Real operator session identity for this fixture's native claim ledger.
    #[arg(long, default_value = "")]
    owner: String,
    /// Existing output parent; report is atomically published after each completed run.
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    policy: Option<String>,
    #[arg(long)]
    today: Option<String>,
    /// Whole-review limit per cold/warm run, plus 120 seconds for cancellation cleanup.
    #[arg(long, default_value_t = 3600)]
    timeout_seconds: u64,
    #[command(flatten)]
    inputs: Inputs,
}
#[derive(Args, Default)]
struct Inputs {
    #[arg(long, requires = "registry_sha256")]
    registry: Option<PathBuf>,
    #[arg(long, requires = "registry")]
    registry_sha256: Option<String>,
    #[arg(long, requires = "mutation_runner_sha256")]
    mutation_runner_bundle: Option<PathBuf>,
    #[arg(long, requires = "mutation_runner_bundle")]
    mutation_runner_sha256: Option<String>,
    #[arg(long, requires = "native_runner_pin_sha256")]
    native_runner_pin: Option<PathBuf>,
    #[arg(long, requires = "native_runner_pin")]
    native_runner_pin_sha256: Option<String>,
    #[arg(long, requires = "equivalence_guest_sha256")]
    equivalence_guest: Option<PathBuf>,
    #[arg(long, requires = "equivalence_guest")]
    equivalence_guest_sha256: Option<String>,
    #[arg(long, requires = "equivalence_store_sha256")]
    equivalence_store: Option<PathBuf>,
    #[arg(long, requires = "equivalence_store")]
    equivalence_store_sha256: Option<String>,
    #[arg(long)]
    equivalence_artifacts: Option<PathBuf>,
}
impl Inputs {
    fn append(&self, command: &mut Command) {
        for (flag, path) in [
            ("--registry", &self.registry),
            ("--mutation-runner-bundle", &self.mutation_runner_bundle),
            ("--native-runner-pin", &self.native_runner_pin),
            ("--equivalence-guest", &self.equivalence_guest),
            ("--equivalence-store", &self.equivalence_store),
            ("--equivalence-artifacts", &self.equivalence_artifacts),
        ] {
            if let Some(path) = path {
                command.arg(flag).arg(path);
            }
        }
        for (flag, value) in [
            ("--registry-sha256", &self.registry_sha256),
            ("--mutation-runner-sha256", &self.mutation_runner_sha256),
            ("--native-runner-pin-sha256", &self.native_runner_pin_sha256),
            ("--equivalence-guest-sha256", &self.equivalence_guest_sha256),
            ("--equivalence-store-sha256", &self.equivalence_store_sha256),
        ] {
            if let Some(value) = value {
                command.arg(flag).arg(value);
            }
        }
    }
}
pub fn run(args: BenchmarkArgs) -> Result<u8> {
    match args.action {
        Action::Prepare(args) => {
            let manifest = fixture::prepare(&args.repo, &args.fixture, &args.scenario, args.kind)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"schema_version":1,"prepared":true,"executed":false,
                "fixture":args.fixture.canonicalize()?,"manifest_sha256":fixture::digest(&crate::bounded_file::read(&args.fixture.join(fixture::MANIFEST),1024*1024)?),"manifest":manifest,"instruction":"Supply operator grants naming this exact fixture source_root, then run benchmark run --execute."})
                )?
            );
            Ok(0)
        }
        Action::Run(args) => execute(*args),
    }
}
fn command(
    args: &RunArgs,
    root: &Path,
    manifest: &fixture::Manifest,
    scenario: &fixture::Scenario,
    receipt: &Path,
) -> Result<Command> {
    // /proc/self/exe names this loaded CLI, not a mutable PATH alias.
    let mut command = Command::new("/proc/self/exe");
    command
        .env_clear()
        .env("PATH", "")
        .env("LANG", "C.UTF-8")
        .env("GIT_INDEX_FILE", fixture::index_path(root, &scenario.name))
        .args([
            "candidate",
            "benchmark-worker",
            "--execute",
            "--kind",
            "index",
            "--surface",
            "pre-commit",
            "--repo",
        ])
        .arg(root)
        .arg("--base-ref")
        .arg(&manifest.baseline_commit)
        .arg("--expected-tree")
        .arg(&scenario.tree_oid)
        .arg("--profile")
        .arg(&scenario.profile)
        .arg("--measurement-receipt")
        .arg(receipt)
        .arg("--owner")
        .arg(&args.owner)
        .arg("--grants")
        .arg(args.grants.as_ref().context("--grants required")?);
    for (flag, value) in [("--policy", &args.policy), ("--today", &args.today)] {
        if let Some(value) = value {
            command.arg(flag).arg(value);
        }
    }
    args.inputs.append(&mut command);
    command.current_dir(root);
    Ok(command)
}
fn row(
    measured: measure::Measured,
    scenario: &fixture::Scenario,
    run: &str,
    manifest: &fixture::Manifest,
    grants: &str,
    path: &Path,
) -> Result<Value> {
    ensure!(
        (0..=1).contains(&measured.exit),
        "review infrastructure exit {}; stderr={}",
        measured.exit,
        String::from_utf8_lossy(&measured.stderr)
    );
    let completion = crate::tool_schema::parse_document(std::str::from_utf8(&measured.stdout)?)?;
    let raw = crate::bounded_file::read(path, crate::candidate_receipt::MAX_BYTES)?;
    ensure!(
        completion == json!({"schema_version":1,"receipt_sha256":fixture::digest(&raw)}),
        "missing/malformed measurement completion or receipt drift"
    );
    let receipt = crate::candidate_receipt::parse_and_verify(&raw)?;
    let report = receipt.payload();
    ensure!(
        report["executed"] == true
            && report["profile"] == scenario.profile
            && report["surface"] == "pre-commit",
        "measurement is not the requested executed profile/surface"
    );
    ensure!(
        report["candidate"]["tree_oid"] == scenario.tree_oid
            && report["candidate"]["base_tree_oid"] == manifest.baseline_tree,
        "measurement candidate/base differs from prepared scenario"
    );
    ensure!(
        report["grants_sha256"] == grants,
        "review grants drifted between admission and measurement"
    );
    ensure!(
        (measured.exit == 0) == (report["decision"] == "pass"),
        "review status/decision mismatch"
    );
    ensure!(
        report["candidate"]["kind"] == "index"
            && report["candidate"]["base_commit_oid"] == manifest.baseline_commit,
        "measurement is not the prepared index/base commit"
    );
    ensure!(
        report["execution_scope"] == "complete_active_profile"
            && report["approval"] == false
            && report["review_complete"].is_boolean()
            && report["execution_verified"] == report["review_complete"],
        "measurement completeness/authority fields are malformed"
    );
    let findings = report["findings"]
        .as_array()
        .context("review findings missing")?;
    let mut counts = BTreeMap::<String, u64>::new();
    let mut summary = Vec::new();
    for finding in findings {
        let severity = finding["severity"]
            .as_str()
            .context("finding severity missing")?;
        *counts.entry(severity.into()).or_default() += 1;
        let message = finding["message"]
            .as_str()
            .context("finding message missing")?;
        summary.push(
            json!({"severity":severity,"check_id":finding["check_id"],"rule_id":finding["rule_id"],
            "path":finding["path"],"message":message.chars().take(2000).collect::<String>()}),
        );
    }
    Ok(
        json!({"run":run,"profile":scenario.profile,"exit_code":measured.exit,"decision":report["decision"],
        "wall_seconds":measured.wall_seconds,"max_rss_kib":measured.max_rss_kib,
        "max_rss_scope":"largest individual waited-process RSS peak reported by Linux wait4; descendant peaks propagate only through completed waits; unpropagated descendants excluded; not simultaneous aggregate tree RSS",
        "engine_duration_ms":report["timings"]["duration_ms"],"cache":report["cache"],
        "review_complete":report["review_complete"],"execution_verified":report["execution_verified"],
        "finding_counts":counts,"findings":summary,"receipt_id":receipt.receipt_id(),"tree_oid":scenario.tree_oid,
        "receipt_path":path,"receipt_sha256":fixture::digest(&raw)}),
    )
}
fn publish(path: &Path, value: &Value) -> Result<()> {
    crate::json_canon::encoded_size(value, 16 * 1024 * 1024)?;
    crate::candidate_report_store::publish(path, &serde_json::to_vec(value)?)
}
fn create_claim(root: &Path, manifest: &fixture::Manifest, owner: &str) -> Result<Claim> {
    let mut paths = vec![
        "benchmark_fixture".to_owned(),
        "native/benchmark_probe".to_owned(),
        conductor_native::candidate_benchmark::python_path(&manifest.package_root)
            .map_err(anyhow::Error::msg)?,
        "package-lock.json".to_owned(),
        format!(
            "{}/candidate_review/test_benchmark_probe.c",
            manifest.package_root
        ),
    ];
    paths.sort();
    paths.dedup();
    let claim = crate::ownership_lifecycle::create(
        root,
        crate::ownership_lifecycle::Create {
            owner,
            paths: &paths,
            justification: "isolated candidate-review latency benchmark",
            expected_minutes: 60.0,
            max_minutes: 120.0,
        },
    )?;
    Ok(Claim {
        root: root.into(),
        owner: owner.into(),
        id: Some(claim.claim_id),
    })
}
fn execute(args: RunArgs) -> Result<u8> {
    ensure!(
        (1..=3600).contains(&args.timeout_seconds),
        "benchmark timeout must be 1..3600 seconds"
    );
    let root = args.fixture.canonicalize()?;
    let manifest = fixture::load(&root)?;
    let manifest_raw = crate::bounded_file::read(&root.join(fixture::MANIFEST), 1024 * 1024)?;
    let manifest_sha = fixture::digest(&manifest_raw);
    let mut output = json!({"schema_version":2,"workload_revision":manifest.workload_revision,"executed":false,
        "planned_at":crate::instant::isoformat_utc(crate::instant::now()),"measured_at":null,
        "host":{"os":std::env::consts::OS,"architecture":std::env::consts::ARCH,"cpu_count":std::thread::available_parallelism()?.get()},
        "fixture":root,"source_kind":manifest.source_kind,"source_head":manifest.source_head,"source_tree":manifest.source_tree,
        "benchmark_baseline_commit":manifest.baseline_commit,"benchmark_baseline_tree":manifest.baseline_tree,
        "manifest_sha256":manifest_sha,"scenario_inventory":manifest.scenarios,
        "run_order":["cold","warm"],"scenarios":{},"status":"planned","approval":false,
        "cache_semantics":"cold/warm are ordered fresh processes; native review cache remains disabled",
        "measurement_scope":"elapsed child lifecycle; max_rss_kib is the largest individual waited-process RSS peak propagated through completed waits, not simultaneous aggregate tree RSS"});
    let output_parent = args
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    ensure!(
        !output_parent.starts_with(&root),
        "benchmark report must be outside fixture"
    );
    if !args.execute {
        publish(&args.output, &output)?;
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(0);
    }
    ensure!(
        !args.owner.trim().is_empty(),
        "--owner required for benchmark execution"
    );
    let source = Source::open(&root)?;
    let grants = Grants::load(
        args.grants
            .as_deref()
            .context("--execute requires --grants")?,
        &source,
    )?;
    ensure!(
        grants.batch.is_some(),
        "native review requires explicit operator batch budget"
    );
    ensure!(
        args.fixture_sha256.as_deref() == Some(manifest_sha.as_str()),
        "--execute requires matching --fixture-sha256 from preparation"
    );
    let interrupts = crate::process_signals::Interrupts::install()?;
    output["executed"] = json!(true);
    output["measured_at"] = json!(crate::instant::isoformat_utc(crate::instant::now()));
    output["status"] = json!("running");
    output["grants_sha256"] = json!(grants.digest());
    let receipts = receipts_directory(&args.output)?;
    output["receipt_directory"] = json!(receipts);
    let held_receipts = crate::candidate_inputs::DirectoryBinding::open(&receipts, Some(&root))?;
    publish(&args.output, &output)?;
    let mut execution = Execution {
        args: &args,
        root: &root,
        manifest: &manifest,
        grants: &grants,
        source: &source,
        manifest_raw: &manifest_raw,
        receipts: &receipts,
        held_receipts: &held_receipts,
        interrupts: &interrupts,
    };
    if let Err(error) = execution.run(&mut output) {
        output["status"] = json!("incomplete");
        if output["failure"].is_null() {
            output["failure"] = json!({"error":format!("{error:#}")});
        }
        publish(&args.output, &output)?;
        return Err(error);
    }
    output["status"] = json!("complete");
    publish(&args.output, &output)?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(0)
}

struct Claim {
    root: PathBuf,
    owner: String,
    id: Option<String>,
}
impl Claim {
    fn release(&mut self) -> Result<()> {
        if let Some(id) = &self.id {
            let released = crate::ownership_lifecycle::release(&self.root, id, &self.owner)?;
            ensure!(released, "benchmark claim disappeared before release");
            self.id = None;
        }
        Ok(())
    }
}
impl Drop for Claim {
    fn drop(&mut self) {
        if let Err(error) = self.release() {
            eprintln!("benchmark claim cleanup: {error:#}");
        }
    }
}

/// Internal data transport; calling it grants no permissions beyond review itself.
#[derive(Args)]
pub struct WorkerArgs {
    #[command(flatten)]
    review: crate::candidate_review::ReviewArgs,
    #[arg(long)]
    measurement_receipt: PathBuf,
}
pub fn worker(args: WorkerArgs, stamp: &BuildStamp) -> Result<u8> {
    let report = crate::candidate_review::run(args.review, stamp)?;
    ensure!(
        report["executed"] == true,
        "benchmark worker requires --execute"
    );
    let raw = serde_json::to_vec(&report)?;
    crate::candidate_report_store::publish(&args.measurement_receipt, &raw)?;
    println!(
        "{}",
        serde_json::to_string(&json!({"schema_version":1,"receipt_sha256":fixture::digest(&raw)}))?
    );
    Ok(u8::from(report["decision"] != "pass"))
}
fn receipts_directory(output: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    let path = parent.join(crate::resource_id("forge-benchmark-receipts")?);
    std::fs::DirBuilder::new().mode(0o700).create(&path)?;
    Ok(path)
}

struct Execution<'a> {
    args: &'a RunArgs,
    root: &'a Path,
    manifest: &'a fixture::Manifest,
    grants: &'a Grants,
    source: &'a Source,
    manifest_raw: &'a [u8],
    receipts: &'a Path,
    held_receipts: &'a crate::candidate_inputs::DirectoryBinding,
    interrupts: &'a crate::process_signals::Interrupts,
}
impl Execution<'_> {
    fn observe(&self, scenario: &fixture::Scenario, run: &str) -> Result<Value> {
        ensure!(
            crate::bounded_file::read(&self.root.join(fixture::MANIFEST), 1024 * 1024)?
                == self.manifest_raw,
            "benchmark manifest changed"
        );
        fixture::load(self.root)?;
        ensure!(
            Grants::load(
                self.args
                    .grants
                    .as_deref()
                    .context("missing benchmark grants")?,
                self.source
            )?
            .digest()
                == self.grants.digest(),
            "benchmark grants changed"
        );
        let receipt = self.receipts.join(format!("{}-{run}.json", scenario.name));
        ensure!(
            !receipt.try_exists()?,
            "measurement receipt already exists; refusing reuse"
        );
        self.held_receipts.verify()?;
        let measured = measure::run(
            &mut command(self.args, self.root, self.manifest, scenario, &receipt)?,
            Duration::from_secs(self.args.timeout_seconds),
            self.interrupts,
        )?;
        self.held_receipts.verify()?;
        let result = row(
            measured,
            scenario,
            run,
            self.manifest,
            self.grants.digest(),
            &receipt,
        )?;
        ensure!(
            crate::bounded_file::read(&self.root.join(fixture::MANIFEST), 1024 * 1024)?
                == self.manifest_raw,
            "benchmark manifest changed during review"
        );
        ensure!(
            Grants::load(
                self.args
                    .grants
                    .as_deref()
                    .context("missing benchmark grants")?,
                self.source
            )?
            .digest()
                == self.grants.digest(),
            "benchmark grants changed during review"
        );
        fixture::load(self.root)?;
        Ok(result)
    }
    fn run(&mut self, output: &mut Value) -> Result<()> {
        for scenario in &self.manifest.scenarios {
            for run in ["cold", "warm"] {
                let mut claim = create_claim(self.root, self.manifest, &self.args.owner)?;
                let observed = self.observe(scenario, run);
                match observed {
                    Ok(row) => {
                        let rows = output["scenarios"]
                            .as_object_mut()
                            .unwrap()
                            .entry(&scenario.name)
                            .or_insert_with(|| json!([]));
                        rows.as_array_mut().unwrap().push(row);
                        publish(&self.args.output, output)?;
                        claim.release()?;
                    }
                    Err(error) => {
                        output["status"] = json!("incomplete");
                        output["failure"] = json!({"scenario":scenario.name,"run":run,"error":format!("{error:#}"),
                        "recovery":"fixture retained; forced cleanup failure requires operator inspection of granted scratch/cgroup roots"});
                        publish(&self.args.output, output)?;
                        claim
                            .release()
                            .context("release benchmark claim after measurement failure")?;
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }
}
