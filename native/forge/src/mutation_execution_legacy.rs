//! Shared confined owner for explicit root and full-source workspace Cargo profiles.
use crate::{
    candidate_inputs::{self, BoundFile},
    mutation_bytecode::isolated_python_env,
    mutation_campaign::{Campaign, Step},
    mutation_execution::{Capture, CommandSession},
    mutation_execution_grants::Grants,
    mutation_process::CommandResult,
    mutation_run::EngineRun,
    mutation_snapshot::Snapshot,
    sandbox::{Policy, Sandbox},
};
use anyhow::{ensure, Context, Result};
use std::{
    cell::Cell,
    collections::BTreeMap,
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
};
fn error(value: impl std::fmt::Display) -> String {
    value.to_string()
}
fn table<'a>(
    value: &'a toml::Value,
    label: &str,
) -> Result<&'a toml::map::Map<String, toml::Value>> {
    value
        .as_table()
        .with_context(|| format!("{label} must be a table"))
}
fn keys(table: &toml::map::Map<String, toml::Value>, allowed: &[&str], label: &str) -> Result<()> {
    ensure!(
        table.keys().all(|key| allowed.contains(&key.as_str())),
        "unsupported {label} field"
    );
    Ok(())
}
fn parse_toml(bytes: &[u8]) -> Result<toml::Value> {
    Ok(std::str::from_utf8(bytes)?.parse::<toml::Value>()?)
}
fn validate_campaign(campaign: &Campaign) -> Result<()> {
    use std::collections::BTreeSet;
    ensure!(
        campaign.generated_value.is_none()
            && campaign.language == "rust"
            && campaign.mutation_engine == "cargo-mutants",
        "ordinary execution currently supports only std-only root Cargo"
    );
    ensure!(
        campaign.environment.is_empty(),
        "ordinary Cargo execution refuses campaign environment overrides"
    );
    ensure!(
        campaign.jobs == 1 && campaign.exclude.is_empty() && campaign.operators.is_empty(),
        "ordinary Cargo execution requires one worker and no generator exclusions or operators"
    );
    ensure!(
        campaign
            .options
            .keys()
            .all(|key| ["manifest_path", "package", "package_root"].contains(&key.as_str())),
        "unsupported Cargo generator option"
    );
    ensure!(
        campaign.option_text("manifest_path").as_deref() == Some("Cargo.toml")
            && campaign.option_text("package_root").is_none(),
        "ordinary Cargo requires root Cargo.toml"
    );
    let selected: BTreeSet<_> = campaign.source_sha256.keys().map(String::as_str).collect();
    let declared: BTreeSet<_> = campaign.source.iter().map(String::as_str).collect();
    ensure!(
        selected == declared && declared.len() == campaign.source.len(),
        "Cargo generator source differs from pinned selected scope"
    );
    Ok(())
}
fn validate_manifest(campaign: &Campaign, bytes: &[u8]) -> Result<(String, String, String)> {
    let cargo = parse_toml(bytes)?;
    let cargo = table(&cargo, "Cargo manifest")?;
    keys(cargo, &["package", "test"], "Cargo manifest")?;
    let package = table(
        cargo.get("package").context("Cargo package missing")?,
        "package",
    )?;
    keys(package, &["name", "version", "edition"], "package")?;
    let name = package
        .get("name")
        .and_then(toml::Value::as_str)
        .context("package name missing")?;
    let version = package
        .get("version")
        .and_then(toml::Value::as_str)
        .context("package version missing")?;
    ensure!(
        campaign.option_text("package").as_deref() == Some(name),
        "Cargo package differs from campaign"
    );
    let expected = vec![
        "cargo".to_string(),
        "test".into(),
        "--manifest-path".into(),
        "Cargo.toml".into(),
        "--package".into(),
        name.into(),
    ];
    ensure!(
        campaign.test_argv == expected,
        "ordinary Cargo test command is unsupported"
    );
    let tests = cargo
        .get("test")
        .and_then(toml::Value::as_array)
        .context("explicit integration target required")?;
    ensure!(
        tests.len() == 1,
        "exactly one explicit integration target is supported"
    );
    let test = table(&tests[0], "test target")?;
    keys(test, &["name", "path", "harness"], "test target")?;
    let test_path = test
        .get("path")
        .and_then(toml::Value::as_str)
        .context("test target path missing")?;
    crate::candidate_source::safe_path(test_path.as_bytes())?;
    ensure!(
        test_path.starts_with("tests/")
            && test_path.ends_with(".rs")
            && test
                .get("name")
                .and_then(toml::Value::as_str)
                .is_some_and(|s| !s.is_empty())
            && test
                .get("harness")
                .is_none_or(|v| v.as_bool() == Some(true)),
        "unsupported integration target"
    );
    ensure!(
        campaign.test_sha256.len() == 1 && campaign.test_sha256.contains_key(test_path),
        "pinned tests differ from integration target"
    );
    Ok((
        name.into(),
        version.into(),
        test["name"].as_str().unwrap().into(),
    ))
}
fn validate_lock(bytes: &[u8], name: &str, version: &str) -> Result<()> {
    let locked = parse_toml(bytes)?;
    let locked = table(&locked, "Cargo lock")?;
    keys(locked, &["version", "package"], "Cargo lock")?;
    let packages = locked
        .get("package")
        .and_then(toml::Value::as_array)
        .context("lock packages missing")?;
    ensure!(
        packages.len() == 1,
        "ordinary Cargo requires a dependency-free lockfile"
    );
    let only = table(&packages[0], "lock package")?;
    keys(only, &["name", "version"], "lock package")?;
    ensure!(
        only.get("name").and_then(toml::Value::as_str) == Some(name)
            && only.get("version").and_then(toml::Value::as_str) == Some(version),
        "lock package differs from manifest"
    );
    Ok(())
}
/// Hold the two root Cargo inputs used for std-only admission.
pub(crate) struct RootInputs {
    manifest: BoundFile,
    lock: BoundFile,
    manifest_hash: String,
    lock_hash: String,
    test_name: String,
    supporting_host: crate::mutation_cargo_supporting::Held,
    supporting_snapshot: std::cell::OnceCell<crate::mutation_cargo_supporting::Held>,
}
impl RootInputs {
    pub fn capture(campaign: &Campaign, root: &Path) -> Result<Self> {
        validate_campaign(campaign)?;
        let (manifest, manifest_bytes) =
            candidate_inputs::repository_live(&root.join("Cargo.toml"), 1024 * 1024)?;
        let (lock, lock_bytes) =
            candidate_inputs::repository_live(&root.join("Cargo.lock"), 1024 * 1024)?;
        let mut required = campaign.source_sha256.clone();
        for (path, hash) in &campaign.test_sha256 {
            if required.insert(path.clone(), hash.clone()).is_some() {
                anyhow::bail!("source and test supporting pins overlap: {path}");
            }
        }
        let supporting_host =
            crate::mutation_cargo_supporting::Held::capture(root, 4096, &required)?;
        let (name, version, test_name) = validate_manifest(campaign, &manifest_bytes)?;
        validate_lock(&lock_bytes, &name, &version)?;
        Ok(Self {
            manifest,
            lock,
            manifest_hash: crate::mutation_campaign::sha256_hex(&manifest_bytes),
            lock_hash: crate::mutation_campaign::sha256_hex(&lock_bytes),
            test_name,
            supporting_host,
            supporting_snapshot: std::cell::OnceCell::new(),
        })
    }
    pub fn verify(&self, snapshot: Option<&Path>) -> Result<()> {
        self.manifest.verify()?;
        self.lock.verify()?;
        self.supporting_host.verify()?;
        if let Some(root) = snapshot {
            for (path, expected) in [
                ("Cargo.toml", &self.manifest_hash),
                ("Cargo.lock", &self.lock_hash),
            ] {
                let (held, bytes) =
                    candidate_inputs::repository_live(&root.join(path), 1024 * 1024)?;
                ensure!(
                    crate::mutation_campaign::sha256_hex(&bytes) == *expected,
                    "snapshot Cargo input drifted: {path}"
                );
                held.verify()?;
            }
        }
        Ok(())
    }
    pub fn supporting_identity(
        &self,
    ) -> &conductor_native::mutation_cargo_supporting_binding::SupportingInputs {
        self.supporting_host.inventory()
    }
    pub fn verify_snapshot(&self, root: &Path) -> Result<()> {
        self.verify(Some(root))?;
        if self.supporting_snapshot.get().is_none() {
            let held = self.supporting_host.capture_peer(root)?;
            ensure!(
                held.inventory() == self.supporting_host.inventory(),
                "snapshot supporting inputs differ from host"
            );
            self.supporting_snapshot.set(held).map_err(|_| {
                anyhow::anyhow!("snapshot supporting inventory already initialized")
            })?;
        }
        self.supporting_snapshot
            .get()
            .expect("supporting inventory initialized")
            .verify()?;
        for (path, kind) in crate::mutation_cargo_capture::names(root)? {
            if conductor_native::mutation_cargo_inventory::excluded(&path) {
                continue;
            }
            ensure!(
                kind != crate::mutation_cargo_capture::Kind::Other,
                "ordinary Cargo refuses symlinks or special inputs: {path}"
            );
            ensure!(
                !path.split('/').any(|part| matches!(
                    part,
                    ".cargo"
                        | ".cargo-mutants.toml"
                        | "build.rs"
                        | "rust-toolchain"
                        | "rust-toolchain.toml"
                ) || part == "Cargo.toml" && path != "Cargo.toml"
                    || part == "Cargo.lock" && path != "Cargo.lock"),
                "ordinary Cargo refuses unsupported configuration: {path}"
            );
        }
        Ok(())
    }
}
/// Profile is chosen from the validated manifest and separately granted by the operator.
pub(crate) enum Inputs {
    Root(Box<RootInputs>),
    Workspace(Box<crate::mutation_workspace_inputs::Inputs>),
}
impl Inputs {
    pub fn capture(campaign: &Campaign, root: &Path, grants: &Grants) -> Result<Self> {
        match campaign
            .options
            .get("execution_profile")
            .and_then(serde_json::Value::as_str)
        {
            None => {
                ensure!(
                    grants.cargo_workspace.is_none(),
                    "workspace grant requires workspace profile"
                );
                RootInputs::capture(campaign, root).map(|inputs| Self::Root(Box::new(inputs)))
            }
            Some("cargo-workspace-v2") => {
                crate::mutation_workspace_inputs::Inputs::capture(campaign, root, grants)
                    .map(|inputs| Self::Workspace(Box::new(inputs)))
            }
            Some(_) => anyhow::bail!("unsupported Cargo execution profile"),
        }
    }
    pub fn populate(&self, snapshot: &mut Snapshot, root: &Path) -> Result<()> {
        match self {
            Self::Root(_) => snapshot.populate(root).map_err(anyhow::Error::msg),
            Self::Workspace(inputs) => inputs.populate(snapshot),
        }
    }
    pub fn verify(&self, snapshot: Option<&Path>) -> Result<()> {
        match self {
            Self::Root(inputs) => inputs.verify(snapshot),
            Self::Workspace(inputs) => inputs.verify(snapshot),
        }
    }
    pub fn verify_snapshot(&self, root: &Path) -> Result<()> {
        match self {
            Self::Root(inputs) => inputs.verify_snapshot(root),
            Self::Workspace(inputs) => inputs.verify(Some(root)),
        }
    }
    pub fn supporting_identity(
        &self,
    ) -> &conductor_native::mutation_cargo_supporting_binding::SupportingInputs {
        match self {
            Self::Root(inputs) => inputs.supporting_identity(),
            Self::Workspace(inputs) => inputs.identity(),
        }
    }
    pub fn receipt(&self, relative: &str) -> Result<()> {
        match self {
            Self::Root(_) => Ok(()),
            Self::Workspace(inputs) => inputs.receipt(relative),
        }
    }
    fn stage(&self, path: &Path) -> Result<Option<crate::mutation_cargo_seed::StagedSeed>> {
        match self {
            Self::Root(_) => Ok(None),
            Self::Workspace(inputs) => inputs.stage(path),
        }
    }
}
/// Own sandbox, reactor and cleanup ledger for one admitted snapshot.
pub(crate) struct Runtime<'a> {
    grants: &'a Grants,
    inputs: &'a Inputs,
    sandbox: Sandbox,
    reactor: tokio::runtime::Runtime,
    programs: BTreeMap<String, PathBuf>,
    root: PathBuf,
    seed: Option<crate::mutation_cargo_seed::StagedSeed>,
    cleanup_complete: Cell<bool>,
    signal: Cell<Option<i32>>,
}
impl<'a> Runtime<'a> {
    pub fn prepare(snapshot: &Snapshot, grants: &'a Grants, inputs: &'a Inputs) -> Result<Self> {
        ensure!(
            tokio::runtime::Handle::try_current().is_err(),
            "mutation execution requires a synchronous owner"
        );
        grants.verify()?;
        grants.validate_private_root(&snapshot.root)?;
        let programs = grants.programs();
        for required in ["cargo", "cargo-mutants", "rustc", "rustdoc"] {
            grants.program(required)?;
        }
        ensure!(
            !programs.contains_key("git"),
            "Git payloads are not supported by this profile"
        );
        let root = snapshot.root.canonicalize()?;
        let environment = BTreeMap::from([
            (
                "PATH".into(),
                root.join("bin").to_string_lossy().into_owned(),
            ),
            (
                "CARGO".into(),
                grants.program("cargo")?.to_string_lossy().into_owned(),
            ),
            (
                "RUSTC".into(),
                grants.program("rustc")?.to_string_lossy().into_owned(),
            ),
            (
                "RUSTDOC".into(),
                grants.program("rustdoc")?.to_string_lossy().into_owned(),
            ),
            (
                "CARGO_HOME".into(),
                root.join("cargo-home").to_string_lossy().into_owned(),
            ),
            (
                "CARGO_TARGET_DIR".into(),
                root.join("target").to_string_lossy().into_owned(),
            ),
            ("CARGO_NET_OFFLINE".into(), "true".into()),
            ("CARGO_BUILD_JOBS".into(), "1".into()),
            ("CARGO_TERM_COLOR".into(), "never".into()),
            ("LANG".into(), "C".into()),
            ("TZ".into(), "UTC".into()),
            (
                "PYTHONPYCACHEPREFIX".into(),
                root.join("pycache").to_string_lossy().into_owned(),
            ),
        ]);
        let sandbox = Sandbox::prepare(Policy {
            read_only: grants.read_only.clone(),
            scratch: root.clone(),
            working_directory: root.clone(),
            environment: environment.clone(),
        })?;
        fs::create_dir(root.join("bin"))?;
        let seed = inputs.stage(&root.join("cargo-home"))?;
        if seed.is_none() {
            fs::create_dir(root.join("cargo-home"))?;
            fs::set_permissions(root.join("cargo-home"), fs::Permissions::from_mode(0o700))?;
        }
        fs::create_dir(root.join("target"))?;
        for (alias, path) in &programs {
            symlink(path, root.join("bin").join(alias))?;
        }
        let isolated = isolated_python_env(&environment, &root, &[]).map_err(anyhow::Error::msg)?;
        ensure!(isolated == environment, "unexpected bytecode environment");
        let reactor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(2)
            .build()?;
        Ok(Self {
            grants,
            inputs,
            sandbox,
            reactor,
            programs,
            root,
            seed,
            cleanup_complete: Cell::new(true),
            signal: Cell::new(None),
        })
    }
    pub fn verify(&self, snapshot: Option<&Path>) -> Result<()> {
        self.grants.verify()?;
        self.grants.validate_private_root(&self.root)?;
        if let Some(path) = snapshot {
            if self.workspace() {
                crate::mutation_workspace_inputs::verify_command_ancestors(path)?;
            }
            self.inputs.verify_snapshot(path)?;
        } else {
            self.inputs.verify(None)?;
        }
        if let Some(seed) = &self.seed {
            seed.verify()?;
        } else if self.workspace() {
            crate::mutation_workspace_inputs::verify_unseeded_home(&self.root.join("cargo-home"))?;
        }
        for (alias, target) in &self.programs {
            ensure!(
                fs::read_link(self.root.join("bin").join(alias))? == *target,
                "program alias changed"
            );
        }
        Ok(())
    }
    pub fn test_arguments(&self, campaign: &Campaign) -> Result<(Vec<String>, Vec<String>)> {
        match self.inputs {
            Inputs::Root(inputs) => {
                let mut baseline = campaign.test_argv.clone();
                baseline.extend([
                    "--test".into(),
                    inputs.test_name.clone(),
                    "--locked".into(),
                    "--offline".into(),
                ]);
                Ok((
                    baseline,
                    vec![format!("--cargo-test-arg=--test={}", inputs.test_name)],
                ))
            }
            Inputs::Workspace(inputs) => {
                let tests = inputs.tests()?;
                Ok((tests.baseline_args.clone(), tests.engine_args.clone()))
            }
        }
    }
    pub fn admit(&self, run: &mut EngineRun<'_>) -> Step<()> {
        let Inputs::Workspace(inputs) = self.inputs else {
            return Ok(());
        };
        let argv = inputs.metadata_argv();
        let root = run.worktree.to_owned();
        let timeout = run.campaign.run_timeout_seconds.min(60);
        let (report, stdout) =
            self.command_capture(run, &argv, &root, timeout, Capture::Metadata)?;
        if !report.succeeded() {
            return Err(format!("Cargo metadata failed: {}", report.stderr_tail).into());
        }
        inputs
            .admit_metadata(
                stdout.as_bytes(),
                &root,
                &self.root.join("cargo-home"),
                run.campaign,
            )
            .map_err(|e| format!("workspace metadata admission: {e:#}"))?;
        let tests = inputs.tests().map_err(error)?;
        run.receipt.insert("workspace_execution".into(), serde_json::json!({
            "profile":"cargo-workspace-v2", "package_id":tests.package_id,
            "package":tests.package_name,"targets":tests.targets,"test_scope":tests.test_scope,
            "seed":self.seed.as_ref().map(|s| s.identity()),
            "input_scope":"captured source and selected registry inputs; operator runtime read grants remain separate"
        }));
        Ok(run.write_receipt()?)
    }
    pub fn output(&self) -> PathBuf {
        self.root.join("mutants-output")
    }
    pub fn observe_baseline(&self, run: &mut EngineRun<'_>, stdout: &str) -> Step<()> {
        if !self.workspace() {
            return Ok(());
        }
        let observed =
            conductor_native::mutation_cargo_observations::parse_package_baseline(stdout)?;
        run.receipt.insert(
            "workspace_baseline".into(),
            serde_json::json!({"direct":observed}),
        );
        Ok(run.write_receipt()?)
    }
    pub fn observe_engine_baseline(
        &self,
        run: &mut EngineRun<'_>,
        report: &serde_json::Value,
    ) -> Step<()> {
        let Inputs::Workspace(inputs) = self.inputs else {
            return Ok(());
        };
        let outcomes = report["outcomes"]
            .as_array()
            .ok_or("engine outcomes missing")?;
        let baseline: Vec<_> = outcomes
            .iter()
            .filter(|row| row["scenario"] == "Baseline")
            .collect();
        if baseline.len() != 1 {
            return Err("one engine baseline required".into());
        }
        let row = baseline[0];
        let relative = row["log_path"]
            .as_str()
            .ok_or("engine baseline log missing")?;
        crate::candidate_source::safe_path(relative.as_bytes()).map_err(error)?;
        if !relative.starts_with("log/") || !relative.ends_with(".log") {
            return Err("invalid engine baseline log path".into());
        }
        let path = self.output().join("mutants.out").join(relative);
        let (held, bytes) = candidate_inputs::repository_live(
            &path,
            conductor_native::mutation_cargo_observations::MAX_LOG_BYTES,
        )
        .map_err(error)?;
        let tests = inputs.tests().map_err(error)?;
        let cargo = self
            .grants
            .program("cargo")
            .map_err(error)?
            .to_str()
            .ok_or("Cargo path UTF-8")?;
        let observed =
            conductor_native::mutation_cargo_observations::parse_package_engine_baseline(
                row,
                std::str::from_utf8(&bytes).map_err(error)?,
                cargo,
                &tests.package_name,
                &tests.package_version,
            )?;
        held.verify().map_err(error)?;
        if observed != run.receipt["workspace_baseline"]["direct"] {
            return Err("engine and direct baseline test observations differ".into());
        }
        run.receipt["workspace_baseline"]["engine"] = observed;
        run.receipt["workspace_baseline"]["engine_log_sha256"] =
            serde_json::json!(crate::mutation_campaign::sha256_hex(&bytes));
        Ok(run.write_receipt()?)
    }
    pub fn workspace(&self) -> bool {
        matches!(self.inputs, Inputs::Workspace(_))
    }
    pub fn engine_campaign(&self, campaign: &Campaign) -> Result<Campaign> {
        let mut selected = campaign.clone();
        if let Inputs::Workspace(inputs) = self.inputs {
            selected.source = inputs.tests()?.engine_sources.clone();
        }
        Ok(selected)
    }
    pub fn report_root(&self, campaign: &Campaign) -> Result<String> {
        match self.inputs {
            Inputs::Root(_) => Ok(campaign.option_text("package_root").unwrap_or_default()),
            Inputs::Workspace(inputs) => Ok(inputs.tests()?.report_root.clone()),
        }
    }
    pub fn report_package(&self) -> Result<Option<&str>> {
        match self.inputs {
            Inputs::Root(_) => Ok(None),
            Inputs::Workspace(inputs) => Ok(Some(&inputs.tests()?.package_name)),
        }
    }
    pub fn command(
        &self,
        run: &mut EngineRun<'_>,
        argv: &[String],
        cwd: &Path,
        timeout: i64,
    ) -> Step<(CommandResult, String)> {
        let capture = if self.workspace() && argv.get(1).is_some_and(|s| s == "test") {
            Capture::Strict
        } else {
            Capture::Complete
        };
        self.command_capture(run, argv, cwd, timeout, capture)
    }
    fn command_capture(
        &self,
        run: &mut EngineRun<'_>,
        argv: &[String],
        cwd: &Path,
        timeout: i64,
        capture: Capture,
    ) -> Step<(CommandResult, String)> {
        if self.workspace() && cwd != run.worktree {
            return Err("workspace command must use the verified snapshot cwd".into());
        }
        self.verify(Some(run.worktree)).map_err(error)?;
        let session = CommandSession {
            sandbox: &self.sandbox,
            reactor: &self.reactor,
            cgroup_parent: &self.grants.cgroup_parent,
            limits: self.grants.resources,
            cleanup_complete: &self.cleanup_complete,
            signal: &self.signal,
            programs: Some(&self.programs),
            scratch: &self.root,
            max_wall_seconds: self.grants.max_wall_seconds,
            cargo_value: false,
        };
        let result = session.command(run, argv, cwd, timeout, capture);
        if let Err(failure) = self.verify(Some(run.worktree)) {
            // Preserve the child's bounded diagnostics when post-command input
            // verification also fails (for example, an incomplete Cargo extract).
            let command = result.as_ref().ok().map(|(report, _, _)| report.as_value());
            run.receipt.insert(
                "command_input_failure".into(),
                serde_json::json!({
                    "error":format!("{failure:#}"), "command":command
                }),
            );
            run.write_receipt()?;
            return Err(error(failure).into());
        }
        result.map(|(report, stdout, _)| (report, stdout))
    }
}
