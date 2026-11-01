//! Native host manifest provisioning. Each write has a durable backup and a
//! comparison against the exact snapshot used to plan it. No services are started.
use crate::config_store::{self as store, FileState};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use conductor_native::project_config;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub(crate) struct ProjectArgs {
    #[arg(long, default_value = ".", global = true)]
    host: PathBuf,
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Show the selected manifest, key inventory and resolved paths, without its text.
    Show,
    /// Create forge.toml from existing host settings or defaults. Existing native
    /// manifests are validated and preserved. Default is a read-only plan.
    Init {
        #[arg(long)]
        apply: bool,
    },
    /// Restore the last initialization snapshot only if it has not drifted.
    Rollback {
        #[arg(long)]
        apply: bool,
    },
}

fn summary(state: &Option<FileState>) -> Value {
    state.as_ref().map_or(Value::Null, |state| {
        json!({
            "sha256":format!("{:x}", Sha256::digest(state.text.as_bytes())),
            "bytes":state.text.len(), "mode":state.mode
        })
    })
}

fn document(root: &Path, source: &Option<FileState>) -> Result<FileState> {
    let legacy = root.join("pyproject.toml");
    let text = source.as_ref().map_or("", |state| state.text.as_str());
    let config = project_config::parse_legacy(&legacy, text).map_err(anyhow::Error::msg)?;
    let mut table = toml::Table::new();
    table.insert("schema_version".into(), toml::Value::Integer(1));
    table.insert("conductor".into(), toml::Value::Table(config.conductor));
    let text = toml::to_string_pretty(&table)?;
    validate(&root.join("forge.toml"), &text)?;
    Ok(FileState { text, mode: 0o600 })
}

fn validate(path: &Path, text: &str) -> Result<()> {
    let config = project_config::parse_native(path, text).map_err(anyhow::Error::msg)?;
    conductor_native::project_paths::validate_config(&config).map_err(anyhow::Error::msg)?;
    crate::session_policy::from_config(&config)?;
    Ok(())
}

fn init(root: &Path, apply: bool) -> Result<Value> {
    let relative = Path::new("forge.toml");
    store::parents(root, relative, false)?;
    let path = root.join(relative);
    let before = store::read(&path)?;
    let source_path = root.join("pyproject.toml");
    let (after, source) = if let Some(existing) = &before {
        validate(&path, &existing.text)?;
        (before.clone(), None)
    } else {
        let source = store::read(&source_path)?;
        (Some(document(root, &source)?), source)
    };
    if apply && before != after {
        let _lock = store::lock_named(&path, "project")?;
        ensure!(
            store::read(&source_path)? == source,
            "legacy host settings changed since planning; retry"
        );
        store::apply(&path, &before, &after)?;
    }
    Ok(json!({"schema":"forge.project-init.v1", "target":path,
        "source": if before.is_some() {path.clone()} else {source_path},
        "source_snapshot":summary(if before.is_some() { &before } else { &source }), "before":summary(&before), "after":summary(&after),
        "changed":before != after, "applied":apply, "creates_services":false}))
}

fn rollback(root: &Path, apply: bool) -> Result<Value> {
    let relative = Path::new("forge.toml");
    store::parents(root, relative, false)?;
    let path = root.join(relative);
    let before = store::read(&path)?;
    let backup = store::read_backup(&path)?;
    ensure!(
        before == backup.after || before == backup.before,
        "native project settings changed after initialization; refusing rollback"
    );
    let after = backup.before;
    if let Some(state) = &after {
        validate(&path, &state.text)?;
    }
    if apply && before != after {
        let _lock = store::lock_named(&path, "project")?;
        ensure!(
            store::read(&path)? == before,
            "project settings changed since planning; retry"
        );
        let current = store::read_backup(&path)?;
        ensure!(
            current.before == after && current.after == before,
            "project backup changed since planning; retry"
        );
        store::restore(&path, &after)?;
    }
    Ok(json!({"schema":"forge.project-rollback.v1", "target":path,
        "before":summary(&before), "after":summary(&after), "changed":before != after, "applied":apply}))
}

fn show(root: &Path) -> Result<Value> {
    let config = project_config::load(root).map_err(anyhow::Error::msg)?;
    let paths = conductor_native::project_paths::resolve(root).map_err(anyhow::Error::msg)?;
    let keys: Vec<_> = config.conductor.keys().collect();
    Ok(
        json!({"schema":"forge.project-config.v1", "host":root, "manifest":config.path,
        "namespace":config.namespace, "keys":keys, "paths":paths,
        "integration_branch":conductor_native::project_paths::integration_branch(root).map_err(anyhow::Error::msg)?}),
    )
}

pub(crate) fn run(args: ProjectArgs) -> Result<u8> {
    let root = args.host.canonicalize().context("resolve host directory")?;
    ensure!(root.is_dir(), "host must be an existing directory");
    let output = match args.action {
        Action::Show => show(&root)?,
        Action::Init { apply } => init(&root, apply)?,
        Action::Rollback { apply } => rollback(&root, apply)?,
    };
    println!("{output}");
    Ok(0)
}
