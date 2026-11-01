//! Native provider hook provisioning with read-only plans and guarded rollback.
use crate::provider::Provider;
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use conductor_native::hook_installer::{retain_hook_commands, shlex_join};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[path = "provider_commands.rs"]
mod commands;
use crate::config_store as store;
use store::FileState;

#[derive(Args)]
pub(crate) struct ProvidersArgs {
    #[arg(long, default_value = ".", global = true)]
    host: PathBuf,
    #[arg(long, value_enum, default_value = "claude", global = true)]
    provider: Provider,
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Plan native hooks; --apply writes settings and a durable rollback snapshot.
    Install {
        #[command(flatten)]
        config: Config,
        #[arg(long)]
        apply: bool,
        /// Replace recognized Forge Python dispatch/startup entries in this config.
        #[arg(long)]
        replace_legacy: bool,
    },
    /// Remove recognized native entries, preserving all other commands.
    Uninstall {
        #[arg(long)]
        apply: bool,
    },
    /// Restore the last snapshot only if settings still match its recorded state.
    Rollback {
        #[arg(long)]
        apply: bool,
    },
    /// Verify the desired installed entries and bounded native binary probe.
    Check(Config),
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Warn,
    Enforce,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Enforce => "enforce",
        }
    }
}

#[derive(Args)]
struct Config {
    /// Native ELF executable named forge. Defaults to this running binary.
    #[arg(long)]
    binary: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "warn")]
    mode: Mode,
    /// Bind a registered identity; otherwise startup resolves host/env identity.
    #[arg(long)]
    identity: Option<String>,
}

fn binary(config: &Config) -> Result<PathBuf> {
    let path = config
        .binary
        .clone()
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)?
        .canonicalize()?;
    ensure!(
        path.file_name().and_then(|s| s.to_str()) == Some("forge"),
        "provider binary must be named forge"
    );
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        "provider binary must be executable"
    );
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic)?;
    ensure!(
        magic == *b"\x7fELF",
        "provider binary must be a native ELF executable"
    );
    if let Some(identity) = &config.identity {
        conductor_native::a2a_store::validate_identity(identity)?;
    }
    Ok(path)
}

fn desired(provider: Provider, root: &Path, binary: &Path, config: &Config) -> Result<Value> {
    let binary = binary
        .to_str()
        .context("provider binary path must be UTF-8")?;
    let host = root.to_str().context("provider host path must be UTF-8")?;
    let events: Vec<_> = if provider.full_hooks() {
        crate::hooks_install::EVENTS.to_vec()
    } else {
        vec![provider.startup_event()]
    };
    let mut hooks = serde_json::Map::new();
    for event in events {
        let mut argv = vec![format!("FORGE_MODE={}", config.mode.name())];
        if let Some(identity) = &config.identity {
            argv.push(format!("A2A_AGENT_NAME={identity}"));
        }
        argv.push(binary.to_owned());
        if provider.full_hooks() {
            argv.extend(
                ["hook", event, "--host", host, "--protocol", provider.name()].map(str::to_owned),
            );
        } else {
            argv.extend(
                [
                    "mailbox",
                    "--host",
                    host,
                    "startup-hook",
                    "--provider",
                    provider.name(),
                ]
                .map(str::to_owned),
            );
        }
        let timeout = if provider == Provider::Qwen {
            15_000
        } else {
            match event {
                "PreToolUse" => 3,
                "PostToolUse" | "SessionEnd" => 6,
                "SubagentStop" => 5,
                "SessionStart" if provider.full_hooks() => 20,
                _ => 15,
            }
        };
        let mut hook = json!({"type":"command", "command":shlex_join(&argv), "timeout":timeout});
        let mut group = json!({"hooks":[]});
        if provider.full_hooks() {
            group["matcher"] = json!(if event.starts_with("Session") {
                ""
            } else {
                ".*"
            });
        } else {
            hook["name"] = json!("forge-a2a-startup");
        }
        group["hooks"] = json!([hook]);
        hooks.insert(event.to_owned(), json!([group]));
    }
    Ok(Value::Object(hooks))
}

fn parse(state: &Option<FileState>) -> Result<Value> {
    let config = match state {
        Some(state) => forge::tool_schema::parse_document(&state.text)?,
        None => json!({}),
    };
    ensure!(config.is_object(), "provider config must be a JSON object");
    Ok(config)
}

fn merge(
    before: &Option<FileState>,
    provider: Provider,
    wanted: Option<&Value>,
    replace_legacy: bool,
) -> Result<Option<FileState>> {
    let original = parse(before)?;
    let mut config = original.clone();
    let mut legacy = false;
    // Inspect first: the pure transform must not erase interpreted hooks without
    // an explicit migration request, or add duplicate hooks beside known ones.
    retain_hook_commands(&mut config, |hook| {
        commands::owned(hook, provider) != Some(commands::Owned::Native)
    })
    .map_err(anyhow::Error::msg)?;
    for groups in config["hooks"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
    {
        for group in groups.as_array().into_iter().flatten() {
            legacy |= group["hooks"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|hook| commands::owned(hook, provider) == Some(commands::Owned::Legacy));
        }
    }
    ensure!(
        wanted.is_none() || !legacy || replace_legacy,
        "recognized interpreted hooks remain; use --replace-legacy to migrate them"
    );
    if replace_legacy {
        retain_hook_commands(&mut config, |hook| {
            commands::owned(hook, provider) != Some(commands::Owned::Legacy)
        })
        .map_err(anyhow::Error::msg)?;
    }
    if let Some(wanted) = wanted {
        let hooks = config
            .as_object_mut()
            .context("provider root")?
            .entry("hooks")
            .or_insert_with(|| json!({}));
        for (event, groups) in wanted.as_object().context("desired hooks")? {
            let target = hooks
                .as_object_mut()
                .context("hooks object")?
                .entry(event)
                .or_insert_with(|| json!([]));
            target.as_array_mut().context("hook event array")?.extend(
                groups
                    .as_array()
                    .context("desired event array")?
                    .iter()
                    .cloned(),
            );
        }
    }
    if config == original {
        return Ok(before.clone());
    }
    let text = format!("{}\n", serde_json::to_string_pretty(&config)?);
    ensure!(
        text.len() <= store::CONFIG_LIMIT,
        "rendered provider configuration exceeds 1 MiB"
    );
    Ok(Some(FileState {
        text,
        mode: before.as_ref().map_or(0o600, |s| s.mode),
    }))
}

fn summary(
    provider: Provider,
    path: &Path,
    before: &Option<FileState>,
    after: &Option<FileState>,
    applied: bool,
    commands: Option<&Value>,
) -> Value {
    let describe = |state: &Option<FileState>| {
        state.as_ref().map(|s| json!({"sha256":format!("{:x}", Sha256::digest(s.text.as_bytes())), "bytes":s.text.len(), "mode":format!("{:o}",s.mode)}))
    };
    json!({"provider":provider.name(), "path":path, "changed":before != after, "applied":applied,
        "before":describe(before), "after":describe(after), "desired_hooks":commands,
        "capability":if provider.full_hooks() {"native tool guards and lifecycle hooks"} else {"bounded A2A startup only"}})
}

fn check(
    provider: Provider,
    root: &Path,
    state: &Option<FileState>,
    config: Config,
) -> Result<Value> {
    let binary = binary(&config)?;
    let wanted = desired(provider, root, &binary, &config)?;
    ensure!(
        merge(state, provider, Some(&wanted), false)? == *state,
        "installed provider hooks differ from requested configuration"
    );
    ensure!(state.is_some(), "provider configuration is missing");
    let version = probe_binary(&binary, root)?;
    Ok(
        json!({"provider":provider.name(),"configured":true,"binary":binary,"version":version,"services_started":false}),
    )
}

fn probe_binary(binary: &Path, root: &Path) -> Result<String> {
    let mut command = std::process::Command::new(binary);
    command.arg("--version").current_dir(root);
    let probe = forge::process_probe::run(&mut command, Duration::from_secs(2), 4096)?;
    ensure!(
        probe.status.success(),
        "provider binary probe exited {}",
        probe.status
    );
    let version = std::str::from_utf8(&probe.stdout)?.trim();
    ensure!(
        version
            .strip_prefix("forge ")
            .is_some_and(crate::hooks_install::versions_match),
        "provider binary version does not match this Forge: {version}"
    );
    Ok(version.to_owned())
}

pub(crate) fn run(args: ProvidersArgs) -> Result<u8> {
    let root = args.host.canonicalize()?;
    let relative = Path::new(args.provider.config_path());
    store::parents(&root, relative, false)?;
    let path = root.join(relative);
    let before = store::read(&path)?;
    let (after, apply, wanted, rollback) = match args.action {
        Action::Install {
            config,
            apply,
            replace_legacy,
        } => {
            let executable = binary(&config)?;
            if config.binary.is_some() {
                probe_binary(&executable, &root)?;
            }
            let wanted = desired(args.provider, &root, &executable, &config)?;
            (
                merge(&before, args.provider, Some(&wanted), replace_legacy)?,
                apply,
                Some(wanted),
                false,
            )
        }
        Action::Uninstall { apply } => (
            merge(&before, args.provider, None, false)?,
            apply,
            None,
            false,
        ),
        Action::Rollback { apply } => {
            let backup = store::read_backup(&path)?;
            ensure!(
                before == backup.after || before == backup.before,
                "provider settings changed after the saved operation; refusing rollback"
            );
            (backup.before, apply, None, true)
        }
        Action::Check(config) => {
            println!("{}", check(args.provider, &root, &before, config)?);
            return Ok(0);
        }
    };
    if apply && before != after {
        store::parents(&root, relative, true)?;
        let _lock = store::lock(&path)?;
        ensure!(
            store::read(&path)? == before,
            "provider configuration changed since planning; retry"
        );
        if rollback {
            let backup = store::read_backup(&path)?;
            ensure!(
                backup.before == after && backup.after == before,
                "provider backup changed since planning; retry"
            );
            store::restore(&path, &after)?;
        } else {
            store::apply(&path, &before, &after)?;
        }
    }
    println!(
        "{}",
        summary(
            args.provider,
            &path,
            &before,
            &after,
            apply,
            wanted.as_ref()
        )
    );
    Ok(0)
}
