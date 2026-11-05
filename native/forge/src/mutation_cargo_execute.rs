//! Opt-in Cargo value observations from original engine scenarios only.
//! Legacy Cargo execution remains in the parent adapter. No mutants are reapplied.
use super::{engine_argv, located, require_baseline, require_verdict_exit, rows, summary};
use crate::{
    candidate_engine_identity::{executable_file, BinaryIdentity},
    candidate_inputs::{self, BoundFile},
    cgroup::Limits as GroupLimits,
    mutation_bytecode::isolated_python_env,
    mutation_campaign::{sha256_hex, Step, Stop},
    mutation_cargo_capture::Held,
    mutation_execution::{Capture, CommandSession},
    mutation_process::{CommandResult, MemoryCap},
    mutation_run::EngineRun,
    mutation_snapshot::{is_executable, on_path},
    sandbox::{Policy as SandboxPolicy, Sandbox},
};
use conductor_native::{
    mutation_cargo_observations as observations,
    mutation_evidence::{cargo_value, cargo_value_receipt},
};
use serde_json::{json, Value};
use std::{
    cell::Cell,
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    os::unix::{fs::symlink, fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

fn error(value: impl std::fmt::Display) -> String {
    value.to_string()
}
fn absent(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "cannot check forbidden Cargo input {}: {e}",
            path.display()
        )),
        Ok(_) => Err(format!(
            "unsupported ambient Cargo configuration: {}",
            path.display()
        )),
    }
}
fn ancestors(root: &Path) -> Result<(), String> {
    for ancestor in root.ancestors() {
        for config in [
            ".cargo/config",
            ".cargo/config.toml",
            "rust-toolchain",
            "rust-toolchain.toml",
        ] {
            absent(&ancestor.join(config))?;
        }
        if ancestor != root {
            absent(&ancestor.join("Cargo.toml"))?;
        }
    }
    Ok(())
}
fn capture(path: &Path, maximum: usize) -> Result<(BoundFile, Vec<u8>), String> {
    candidate_inputs::repository_live(path, maximum).map_err(error)
}
fn capture_manifest(path: &Path, expected_sha: &str) -> Result<(BoundFile, Value), String> {
    let (held, bytes) = capture(path, observations::MAX_REPORT_BYTES)?;
    if sha256_hex(&bytes) != expected_sha {
        return Err("held campaign bytes differ from Campaign digest".into());
    }
    let manifest = conductor_native::mutation_evidence::parse_request(
        std::str::from_utf8(&bytes).map_err(error)?,
    )?;
    Ok((held, manifest))
}
struct Inputs {
    manifest: Value,
    manifest_file: BoundFile,
    repo: Held,
    snapshot: Held,
}
impl Inputs {
    fn capture(run: &EngineRun) -> Result<Self, String> {
        if run.interpreter.is_some() {
            return Err("opt-in Cargo value evidence currently requires no configured Python guest; legacy Cargo execution remains available".into());
        }
        if !run.campaign.environment.is_empty() {
            return Err(
                "Cargo value execution does not support configured environment overrides".into(),
            );
        }
        let root = run.repo_root.canonicalize().map_err(error)?;
        let named = run.campaign.manifest_path.canonicalize().map_err(error)?;
        named
            .strip_prefix(&root)
            .map_err(|_| "campaign manifest is outside host root")?;
        // Generated campaigns may be Git-ignored. Only the trusted coordinator
        // consumes these held bytes; snapshot source/discovery is captured below.
        let (manifest_file, manifest) = capture_manifest(&named, &run.campaign.manifest_sha256)?;
        if manifest.get("generated_value") != run.campaign.generated_value.as_ref() {
            return Err("Campaign generated value differs from original manifest".into());
        }
        let declaration = cargo_value::declaration_path(&manifest)?
            .ok_or("generated Cargo value declaration absent")?;
        let selected: Vec<_> = run.campaign.source_sha256.keys().cloned().collect();
        let repo = Held::capture(&root, &declaration, &selected).map_err(error)?;
        let snapshot = Held::capture(run.worktree, &declaration, &selected).map_err(error)?;
        for held in [&repo, &snapshot] {
            cargo_value::validate(
                &manifest,
                held.declaration_bytes(),
                &selected,
                held.inventory(),
            )?;
        }
        let input = Self {
            manifest,
            manifest_file,
            repo,
            snapshot,
        };
        input.verify(run)?;
        Ok(input)
    }
    fn verify(&self, run: &EngineRun) -> Result<(), String> {
        self.manifest_file.verify().map_err(error)?;
        self.repo.verify().map_err(error)?;
        self.snapshot.verify().map_err(error)?;
        ancestors(run.repo_root)?;
        ancestors(run.worktree)
    }
}
struct Tool {
    path: PathBuf,
    identity: BinaryIdentity,
    _held: File,
}
impl Tool {
    fn capture(path: &Path) -> Result<Self, String> {
        let path = path.canonicalize().map_err(error)?;
        if !is_executable(&path) {
            return Err(format!("tool is not executable: {}", path.display()));
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(error)?;
        let identity =
            executable_file(file.try_clone().map_err(error)?, || Ok(())).map_err(error)?;
        let tool = Self {
            path,
            identity,
            _held: file,
        };
        tool.verify()?;
        Ok(tool)
    }
    fn verify(&self) -> Result<(), String> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.path)
            .map_err(error)?;
        if executable_file(file, || Ok(())).map_err(error)? != self.identity {
            return Err(format!(
                "Cargo executable input changed: {}",
                self.path.display()
            ));
        }
        Ok(())
    }
    fn evidence(&self) -> Value {
        json!({"path":self.path,"sha256":self.identity.sha256,"bytes":self.identity.bytes,"scope":"executable point observations; not dynamic-library/compiler closure attestation"})
    }
}
struct Runtime {
    directory: Option<tempfile::TempDir>,
    tools: Vec<Tool>,
    environment: BTreeMap<String, String>,
    cargo: String,
    engine: String,
    rustc: String,
    rustdoc: String,
    links: BTreeMap<String, PathBuf>,
    sandbox: Option<Sandbox>,
    reactor: tokio::runtime::Runtime,
    cgroup_parent: PathBuf,
    cleanup_complete: Cell<bool>,
    signal: Cell<Option<i32>>,
    limits: GroupLimits,
    operator_home: PathBuf,
    rustup_home: PathBuf,
}
impl Runtime {
    fn prepare(run: &mut EngineRun, binary: &str) -> Step<Self> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err("Cargo value execution requires its synchronous owner thread".into());
        }
        let cgroup_parent = operator_cgroup_parent()?;
        let (operator_home, rustup_home) = operator_rustup_roots()?;
        let directory = tempfile::Builder::new()
            .prefix("forge-cargo-value-")
            .tempdir()
            .map_err(error)?;
        let root = directory.path();
        let canonical = root.canonicalize().map_err(error)?;
        if root != canonical
            || canonical.starts_with(run.repo_root.canonicalize().map_err(error)?)
            || canonical.starts_with(run.worktree.canonicalize().map_err(error)?)
        {
            return Err("private Cargo runtime must be canonical and outside package roots".into());
        }
        ancestors(root)?;
        fs::create_dir(root.join("bin")).map_err(error)?;
        fs::create_dir(root.join("payload")).map_err(error)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("payload"), fs::Permissions::from_mode(0o700))
            .map_err(error)?;
        let reactor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(2)
            .build()
            .map_err(error)?;
        let mut runtime = Self {
            directory: Some(directory),
            tools: Vec::new(),
            environment: BTreeMap::new(),
            cargo: String::new(),
            engine: String::new(),
            rustc: String::new(),
            rustdoc: String::new(),
            links: BTreeMap::new(),
            sandbox: None,
            reactor,
            cgroup_parent,
            cleanup_complete: Cell::new(true),
            signal: Cell::new(None),
            limits: group_limits(run.supervisor.memory_cap)?,
            operator_home,
            rustup_home,
        };
        run.receipt.insert(
            "cargo_value_runtime".into(),
            json!({"root":runtime.root(),"removed":false,"preparing":true}),
        );
        for (name, path) in toolchain(&runtime.rustup_home, &runtime.operator_home)? {
            let held = Tool::capture(&path)?;
            symlink(&held.path, runtime.root().join("bin").join(&name)).map_err(error)?;
            if matches!(name.as_str(), "cargo" | "rustc" | "rustdoc")
                && held.path.file_name().is_none_or(|n| n != name.as_str())
            {
                return Err("toolchain component resolves to a proxy".into());
            }
            let direct = held.path.to_string_lossy().into_owned();
            match name.as_str() {
                "cargo" => runtime.cargo = direct,
                "rustc" => runtime.rustc = direct,
                "rustdoc" => runtime.rustdoc = direct,
                _ => {}
            }
            runtime.links.insert(name, held.path.clone());
            runtime.tools.push(held);
        }
        let engine = Tool::capture(Path::new(binary))?;
        runtime.engine = engine.path.to_string_lossy().into_owned();
        runtime.tools.push(engine);
        runtime.prepare_sandbox(run)?;
        Ok(runtime)
    }
    fn prepare_sandbox(&mut self, run: &EngineRun) -> Step<()> {
        self.environment = clean_environment(self.root(), &self.cargo, &self.rustc, &self.rustdoc)?;
        self.environment = run.guest_environment(&self.environment);
        // Sandbox must admit an empty scratch BEFORE any trusted parent seeding.
        self.sandbox = Some(
            Sandbox::prepare(SandboxPolicy {
                read_only: read_roots(run.repo_root, run.worktree, self)?,
                scratch: self.payload(),
                working_directory: run.worktree.to_path_buf(),
                environment: self.environment.clone(),
            })
            .map_err(error)?,
        );
        for name in ["cargo-home", "target", "output"] {
            fs::create_dir(self.payload().join(name)).map_err(error)?;
        }
        let isolated =
            isolated_python_env(&self.environment, &self.payload().join("bytecode"), &[])?;
        if isolated != self.environment {
            return Err("Cargo bytecode environment differs from prepared sandbox".into());
        }
        self.verify()?;
        Ok(())
    }
    fn payload(&self) -> PathBuf {
        self.root().join("payload")
    }
    fn root(&self) -> &Path {
        self.directory.as_ref().expect("owned runtime").path()
    }
    fn verify(&self) -> Result<(), String> {
        ancestors(self.root())?;
        for name in ["config", "config.toml"] {
            absent(&self.payload().join("cargo-home").join(name))?;
        }
        for (name, target) in &self.links {
            if fs::read_link(self.root().join("bin").join(name)).map_err(error)? != *target {
                return Err("private native-tool link changed".into());
            }
        }
        for tool in &self.tools {
            tool.verify()?;
        }
        Ok(())
    }
    fn finish(mut self, success: bool) -> Result<bool, String> {
        let directory = self.directory.take().ok_or("runtime already released")?;
        if success && self.cleanup_complete.get() {
            directory.close().map_err(error)?;
            Ok(true)
        } else {
            let preserved = directory.keep();
            eprintln!(
                "forge: retained opt-in Cargo runtime at {}",
                preserved.display()
            );
            Ok(false)
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        // Failure/cancellation/partial preparation retains private runtime data;
        // unresolved process cleanup must never race recursive directory removal.
        if let Some(directory) = self.directory.take() {
            let preserved = directory.keep();
            eprintln!(
                "forge: retained opt-in Cargo runtime at {}",
                preserved.display()
            );
        }
    }
}
fn clean_environment(
    root: &Path,
    cargo: &str,
    rustc: &str,
    rustdoc: &str,
) -> Result<BTreeMap<String, String>, String> {
    Ok(BTreeMap::from([
        (
            "PATH".into(),
            root.join("bin").to_string_lossy().into_owned(),
        ),
        ("CARGO".into(), cargo.into()),
        ("RUSTC".into(), rustc.into()),
        ("RUSTDOC".into(), rustdoc.into()),
        (
            "CARGO_HOME".into(),
            root.join("payload/cargo-home")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "CARGO_TARGET_DIR".into(),
            root.join("payload/target").to_string_lossy().into_owned(),
        ),
        (
            "PYTHONPYCACHEPREFIX".into(),
            root.join("payload/bytecode/pycache")
                .to_string_lossy()
                .into_owned(),
        ),
        ("CARGO_TERM_COLOR".into(), "never".into()),
        ("CARGO_BUILD_JOBS".into(), "1".into()),
        ("CARGO_NET_OFFLINE".into(), "true".into()),
        ("INSTA_UPDATE".into(), "no".into()),
        ("INSTA_FORCE_PASS".into(), "0".into()),
        ("LC_ALL".into(), "C".into()),
        ("LANG".into(), "C".into()),
        ("TZ".into(), "UTC".into()),
    ]))
}
fn group_limits(cap: Option<MemoryCap>) -> Result<GroupLimits, String> {
    let memory_mib = cap.map_or(8192, |cap| (cap.limit / (1024 * 1024)).min(8192));
    if memory_mib < 16 {
        return Err(
            "configured mutation memory cap is below supported 16 MiB cgroup minimum".into(),
        );
    }
    Ok(GroupLimits {
        cpu_millis: 1000,
        memory_mib,
        processes: 64,
    })
}
fn operator_cgroup_parent() -> Result<PathBuf, String> {
    let raw=std::env::var_os("FORGE_MUTATION_CGROUP_PARENT").ok_or("Cargo value execution requires operator FORGE_MUTATION_CGROUP_PARENT naming an existing delegated cgroup v2 parent; no controllers are enabled automatically")?;
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err("FORGE_MUTATION_CGROUP_PARENT must be absolute".into());
    }
    let actual = path.canonicalize().map_err(error)?;
    if actual != path
        || !actual.starts_with("/sys/fs/cgroup")
        || actual == Path::new("/sys/fs/cgroup")
    {
        return Err("operator cgroup parent must be canonical beneath /sys/fs/cgroup".into());
    }
    Ok(actual)
}
fn operator_rustup_roots() -> Result<(PathBuf, PathBuf), String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("operator HOME unavailable for toolchain boundary")?;
    if !home.is_absolute() {
        return Err("operator HOME must be absolute".into());
    }
    let home = home.canonicalize().map_err(error)?;
    let rustup = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".rustup"));
    if !rustup.is_absolute() {
        return Err("operator RUSTUP_HOME must be absolute".into());
    }
    let rustup = rustup.canonicalize().map_err(error)?;
    let toolchains = rustup.join("toolchains");
    if toolchains.canonicalize().map_err(error)? != toolchains {
        return Err("Rustup toolchains directory must be canonical without an alias".into());
    }
    Ok((home, rustup))
}
fn toolchain(rustup_home: &Path, home: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    let cargo=std::env::var_os("CARGO").map(PathBuf::from).ok_or("set CARGO to an absolute direct toolchain Cargo binary, e.g. CARGO=$(rustup which cargo); rustup is not executed by Forge")?;
    if !cargo.is_absolute() {
        return Err("CARGO must name an absolute direct toolchain executable".into());
    }
    let cargo = cargo.canonicalize().map_err(error)?;
    if cargo.file_name().is_none_or(|n| n != "cargo") {
        return Err(
            "CARGO resolves to a proxy; set CARGO=$(rustup which cargo) before invoking Forge"
                .into(),
        );
    }
    narrow_toolchain_root(&cargo, rustup_home, home)?;
    let parent = cargo.parent().ok_or("Cargo toolchain directory absent")?;
    let mut result = BTreeMap::from([
        ("cargo".into(), cargo.clone()),
        ("rustc".into(), parent.join("rustc")),
        ("rustdoc".into(), parent.join("rustdoc")),
    ]);
    for name in [
        "cc", "gcc", "ld", "as", "ar", "ranlib", "strip", "nm", "objcopy", "sh",
    ] {
        result.insert(
            name.into(),
            on_path(name).ok_or_else(|| format!("native tool {name} absent from operator PATH"))?,
        );
    }
    Ok(result)
}
fn narrow_toolchain_root(cargo: &Path, rustup_home: &Path, home: &Path) -> Result<PathBuf, String> {
    let bin = cargo.parent().ok_or("Cargo has no bin directory")?;
    let root = bin.parent().ok_or("Cargo toolchain root unavailable")?;
    if cargo.file_name().is_none_or(|name| name != "cargo")
        || bin.file_name().is_none_or(|name| name != "bin")
        || root.parent() != Some(rustup_home.join("toolchains").as_path())
        || home.starts_with(root)
    {
        return Err("CARGO must name a direct canonical <RUSTUP_HOME>/toolchains/<name>/bin/cargo; broad home/bin and system roots are unsupported".into());
    }
    Ok(root.to_path_buf())
}
fn overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}
fn validate_read_root(
    root: &Path,
    repo: &Path,
    snapshot: &Path,
    runtime: &Path,
) -> Result<(), String> {
    if [repo, snapshot, runtime]
        .iter()
        .any(|excluded| overlaps(root, excluded))
    {
        return Err("read capability overlaps repository, snapshot or runtime".into());
    }
    Ok(())
}
fn read_roots(repo: &Path, snapshot: &Path, runtime: &Runtime) -> Result<Vec<PathBuf>, String> {
    let toolchain = narrow_toolchain_root(
        Path::new(&runtime.cargo),
        &runtime.rustup_home,
        &runtime.operator_home,
    )?;
    let repo = repo.canonicalize().map_err(error)?;
    let snapshot = snapshot.canonicalize().map_err(error)?;
    if overlaps(&repo, &snapshot) {
        return Err("host and snapshot roots overlap".into());
    }
    validate_read_root(&toolchain, &repo, &snapshot, runtime.root())?;
    let mut roots = vec![snapshot.clone(), runtime.root().join("bin"), toolchain];
    roots.extend(runtime.tools.iter().map(|tool| tool.path.clone()));
    // Fixed system runtime grants are intentionally broad; no user HOME,
    // arbitrary executable parent, /proc, /sys or cgroup filesystem grant.
    for path in ["/usr", "/lib", "/lib64", "/etc/ld.so.cache"] {
        let path = Path::new(path);
        if path.exists() {
            roots.push(path.canonicalize().map_err(error)?);
        }
    }
    let bin = runtime.root().join("bin");
    for root in &roots {
        if *root != snapshot && *root != bin {
            validate_read_root(root, &repo, &snapshot, runtime.root())?;
        }
    }
    roots.sort();
    roots.dedup();
    if roots.iter().any(|path| {
        path.starts_with("/sys/fs/cgroup")
            || path == Path::new("/")
            || path.starts_with(runtime.payload())
    }) {
        return Err("invalid read capability for Cargo sandbox".into());
    }
    Ok(roots)
}
impl Runtime {
    fn command(
        &self,
        run: &mut EngineRun,
        argv: &[String],
        seconds: i64,
    ) -> Step<(CommandResult, String, Value)> {
        let cwd = run.worktree.to_owned();
        let session = CommandSession {
            sandbox: self.sandbox.as_ref().ok_or("Cargo sandbox unavailable")?,
            reactor: &self.reactor,
            cgroup_parent: &self.cgroup_parent,
            limits: self.limits,
            cleanup_complete: &self.cleanup_complete,
            signal: &self.signal,
            programs: None,
            scratch: &cwd,
            max_wall_seconds: 86400,
            cargo_value: true,
        };
        session.command(run, argv, &cwd, seconds, Capture::Strict)
    }
}
fn baseline_runs(run: &mut EngineRun, inputs: &Inputs, runtime: &Runtime) -> Step<Vec<Value>> {
    let selected: Vec<_> = run.campaign.source_sha256.keys().cloned().collect();
    let declaration = conductor_native::mutation_rust_value_contract::parse(
        inputs.snapshot.declaration_bytes(),
        &selected,
    )?;
    let mut reports = Vec::new();
    let mut argv = run.campaign.test_argv.clone();
    argv[0] = runtime.cargo.clone();
    let target = target(inputs, runtime);
    run.receipt.insert("baseline_runs".into(), json!([]));
    for index in 0..declaration.baseline_repetitions() {
        inputs.verify(run)?;
        runtime.verify()?;
        let timeout = run.campaign.run_timeout_seconds;
        let (result, stdout, confinement) = runtime.command(run, &argv, timeout)?;
        run.receipt
            .get_mut("baseline_runs")
            .and_then(Value::as_array_mut)
            .ok_or("baseline run inventory missing")?
            .push(json!({"argv":argv,"result":result.as_value(),"confinement":confinement}));
        if index == 0 {
            run.note_baseline(&result, &argv)?;
        }
        if !result.succeeded() {
            return Err("independent Cargo baseline did not pass".into());
        }
        reports.push(observations::parse_baseline(
            &stdout,
            &target,
            inputs.snapshot.inventory().ranked_nodeids(),
        )?);
    }
    Ok(reports)
}
fn target(inputs: &Inputs, runtime: &Runtime) -> observations::Target {
    let t = inputs.snapshot.inventory().target();
    observations::Target {
        cargo_executable: runtime.cargo.clone(),
        package: t.package().into(),
        version: t.version().into(),
        test: t.test().into(),
        test_path: t.path().into(),
    }
}
fn confined_logs(
    output: &Path,
    report: &Value,
) -> Result<(Vec<observations::ScenarioLog>, Vec<BoundFile>), String> {
    let outcomes = report["outcomes"]
        .as_array()
        .ok_or("engine outcomes missing")?;
    if outcomes.len() > observations::MAX_MUTANTS + 1 {
        return Err("engine scenario count exceeds bound".into());
    }
    let mut logs = Vec::new();
    let mut held = Vec::new();
    let mut total = 0usize;
    for scenario in outcomes {
        let relative = scenario["log_path"]
            .as_str()
            .ok_or("engine log path missing")?;
        crate::candidate_source::safe_path(relative.as_bytes()).map_err(error)?;
        if !relative.starts_with("log/") || !relative.ends_with(".log") {
            return Err("invalid engine scenario log path".into());
        }
        let path = output.join("mutants.out").join(relative);
        let (file, bytes) = capture(&path, observations::MAX_LOG_BYTES)?;
        total = total
            .checked_add(bytes.len())
            .ok_or("engine log byte overflow")?;
        if total > observations::MAX_TOTAL_LOG_BYTES {
            return Err("engine scenario logs exceed total bound".into());
        }
        logs.push(observations::ScenarioLog {
            path: relative.into(),
            text: String::from_utf8(bytes).map_err(error)?,
        });
        held.push(file);
    }
    Ok((logs, held))
}
fn expected(
    report: &Value,
    mutants: &[Value],
) -> Result<Vec<observations::ExpectedMutant>, String> {
    let located = located(report)?;
    if located.len() != mutants.len() {
        return Err("engine/Forge mutant inventory mismatch".into());
    }
    located
        .into_iter()
        .zip(mutants)
        .map(|((_, _, _, outcome), row)| {
            let id = row["id"].as_str().ok_or("Forge mutant ID absent")?;
            Ok(observations::ExpectedMutant {
                id: id.into(),
                scenario: outcome["scenario"].clone(),
            })
        })
        .collect()
}
fn retained(observed: &observations::Observations, digest: &str) -> Value {
    let mutants:Vec<_>=observed.mutants.iter().map(|row|json!({"id":row.id,"report":row.report,"log_path":row.log_path,"log_sha256":row.log_sha256,"phases":row.phases})).collect();
    json!({"schema_version":"llm.mutation-testing.cargo-value-observations.v1","inventory_digest":digest,"baseline_reports":observed.baseline_reports,"engine_baseline":observed.engine_baseline,"mutants":mutants})
}
fn execute_owned(run: &mut EngineRun, inputs: &Inputs, runtime: &Runtime) -> Step<()> {
    let baselines = baseline_runs(run, inputs, runtime)?;
    let output = runtime.payload().join("output");
    let mut argv = engine_argv(run.campaign, &runtime.engine, &output)?;
    argv.extend([
        "--no-config".into(),
        "--baseline".into(),
        "run".into(),
        "--cargo-arg=--locked".into(),
        "--cargo-arg=--offline".into(),
        format!(
            "--cargo-test-arg=--test={}",
            inputs.snapshot.inventory().target().test()
        ),
    ]);
    inputs.verify(run)?;
    runtime.verify()?;
    run.receipt.insert("engine_argv".into(), json!(argv));
    let timeout = run.campaign.run_timeout_seconds;
    let (result, _, confinement) = runtime.command(run, &argv, timeout)?;
    run.receipt.insert("engine_confinement".into(), confinement);
    run.receipt
        .insert("engine_result".into(), result.as_value());
    run.receipt.insert("engine_argv".into(), json!(argv));
    if result.timed_out {
        return Err("opt-in cargo-mutants exceeded run timeout".into());
    }
    let (report_file, bytes) = capture(
        &output.join("mutants.out/outcomes.json"),
        observations::MAX_REPORT_BYTES,
    )?;
    let report_text = std::str::from_utf8(&bytes).map_err(error)?;
    let report = conductor_native::mutation_evidence::parse_request(report_text)?;
    run.planned = report["total_mutants"].as_u64();
    require_baseline(&report)?;
    require_verdict_exit(&result)?;
    let mutants = rows(&report, "")?;
    run.receipt.insert(
        "engine_version".into(),
        report["cargo_mutants_version"].clone(),
    );
    run.receipt
        .insert("engine_summary".into(), summary(&report, &mutants));
    run.receipt.insert("mutants".into(), json!(mutants));
    let (logs, held) = confined_logs(&output, &report)?;
    let observed = observations::collect(
        report_text,
        &logs,
        &target(inputs, runtime),
        inputs.snapshot.inventory().ranked_nodeids(),
        &baselines,
        &expected(&report, &mutants)?,
    )?;
    let retained = retained(&observed, inputs.snapshot.inventory().digest());
    let metadata = &inputs.manifest["generated_value"];
    let (value, attribution) = cargo_value_receipt::evaluate(
        metadata,
        &inputs.manifest["source_sha256"],
        &json!(mutants),
        &retained,
    )?;
    run.receipt
        .insert("cargo_value_observations".into(), retained);
    let accepted = value["status"] == "PASS" && attribution["status"] == "ATTRIBUTED";
    run.receipt.insert("test_value".into(), value);
    run.receipt.insert("attribution".into(), attribution);
    if !accepted {
        return Err(
            "original Cargo scenarios did not yield complete PASS test-value evidence".into(),
        );
    }
    report_file.verify().map_err(error)?;
    for file in held {
        file.verify().map_err(error)?;
    }
    inputs.verify(run)?;
    runtime.verify()?;
    run.require_scored_rows()
}
pub(super) fn execute(run: &mut EngineRun, binary: &str) -> Step<()> {
    run.receipt.insert(
        "cargo_value_process_cleanup".into(),
        json!({"complete":true,"commands_observed":0,"scope":"no actor commands launched"}),
    );
    let inputs = Inputs::capture(run)?;
    let runtime = Runtime::prepare(run, binary)?;
    let root = runtime.root().to_path_buf();
    run.receipt.insert("cargo_value_memory_enforcement".into(),json!({"mode":"aggregate_cgroup","bytes":runtime.limits.memory_mib*1024*1024,"legacy_requested_per_process_data_bytes":run.supervisor.memory_cap.map(|cap|cap.limit),"scope":"aggregate resident memory budget, not identical RLIMIT_DATA enforcement"}));
    run.receipt.insert("cargo_value_runtime".into(),json!({"root":root,"removed":false,"tools":runtime.tools.iter().map(Tool::evidence).collect::<Vec<_>>(),"sandbox":runtime.sandbox.as_ref().map(Sandbox::report),"cgroup_parent":runtime.cgroup_parent,"limits":runtime.limits,"effective_home":runtime.payload(),"effective_tmpdir":runtime.payload(),"operator_rustup_home":runtime.rustup_home}));
    let mut result = execute_owned(run, &inputs, &runtime);
    let mut identity_errors = Vec::new();
    for verification in [inputs.verify(run), runtime.verify()] {
        if let Err(error) = verification {
            identity_errors.push(error.clone());
            if result.is_ok() {
                result = Err(Stop::Refused(error));
            }
        }
    }
    if !identity_errors.is_empty() {
        run.receipt
            .insert("cargo_value_identity_errors".into(), json!(identity_errors));
    }
    match runtime.finish(result.is_ok()) {
        Ok(removed) => {
            run.receipt["cargo_value_runtime"]["removed"] = json!(removed);
        }
        Err(error) => {
            run.receipt
                .insert("cargo_value_cleanup_error".into(), json!(error));
            if result.is_ok() {
                result = Err(Stop::Refused(error));
            }
        }
    }
    result
}
#[cfg(test)]
#[path = "mutation_cargo_execute_tests.rs"]
mod tests;
