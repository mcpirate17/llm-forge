//! First admitted ordinary mutation profile: confined std-only root Cargo.
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
    os::unix::fs::symlink,
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
pub(crate) struct Inputs {
    manifest: BoundFile,
    lock: BoundFile,
    manifest_hash: String,
    lock_hash: String,
    test_name: String,
}
impl Inputs {
    pub fn capture(campaign: &Campaign, root: &Path) -> Result<Self> {
        validate_campaign(campaign)?;
        let (manifest, manifest_bytes) =
            candidate_inputs::repository_live(&root.join("Cargo.toml"), 1024 * 1024)?;
        let (lock, lock_bytes) =
            candidate_inputs::repository_live(&root.join("Cargo.lock"), 1024 * 1024)?;
        let (name, version, test_name) = validate_manifest(campaign, &manifest_bytes)?;
        validate_lock(&lock_bytes, &name, &version)?;
        Ok(Self {
            manifest,
            lock,
            manifest_hash: crate::mutation_campaign::sha256_hex(&manifest_bytes),
            lock_hash: crate::mutation_campaign::sha256_hex(&lock_bytes),
            test_name,
        })
    }
    pub fn verify(&self, snapshot: Option<&Path>) -> Result<()> {
        self.manifest.verify()?;
        self.lock.verify()?;
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
    pub fn verify_snapshot(&self, root: &Path) -> Result<()> {
        self.verify(Some(root))?;
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
/// Own sandbox, reactor and cleanup ledger for one admitted snapshot.
pub(crate) struct Runtime<'a> {
    grants: &'a Grants,
    inputs: &'a Inputs,
    sandbox: Sandbox,
    reactor: tokio::runtime::Runtime,
    programs: BTreeMap<String, PathBuf>,
    root: PathBuf,
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
        fs::create_dir(root.join("cargo-home"))?;
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
            cleanup_complete: Cell::new(true),
            signal: Cell::new(None),
        })
    }
    pub fn verify(&self, snapshot: Option<&Path>) -> Result<()> {
        self.grants.verify()?;
        self.grants.validate_private_root(&self.root)?;
        if let Some(path) = snapshot {
            self.inputs.verify_snapshot(path)?;
        } else {
            self.inputs.verify(None)?;
        }
        for (alias, target) in &self.programs {
            ensure!(
                fs::read_link(self.root.join("bin").join(alias))? == *target,
                "program alias changed"
            );
        }
        Ok(())
    }
    pub fn test_name(&self) -> &str {
        &self.inputs.test_name
    }
    pub fn output(&self) -> PathBuf {
        self.root.join("mutants-output")
    }
    pub fn command(
        &self,
        run: &mut EngineRun<'_>,
        argv: &[String],
        cwd: &Path,
        timeout: i64,
    ) -> Step<(CommandResult, String)> {
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
        let result = session.command(run, argv, cwd, timeout, Capture::Complete);
        self.verify(Some(run.worktree)).map_err(error)?;
        result.map(|(report, stdout, _)| (report, stdout))
    }
}
