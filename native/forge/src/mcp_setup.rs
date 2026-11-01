//! Project-scoped MCP configuration for an explicitly authorized running service.
//! Probe first, preserve unrelated settings, then publish with a durable rollback.
use crate::config_store::{self as store, FileState};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use forge::harness::transport;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Args)]
pub(crate) struct McpArgs {
    #[arg(long, default_value = ".", global = true)]
    host: PathBuf,
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Plan a .mcp.json entry for this binary; --apply publishes it.
    Bind {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        apply: bool,
        /// Remove an exact legacy conductor.crg_server entry for this host.
        #[arg(long)]
        replace_legacy: bool,
    },
    /// Verify the installed entry and service source root/schema without starting it.
    Check {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Remove only the recognized native entry; leaves the service running.
    Unbind {
        #[arg(long)]
        apply: bool,
    },
    /// Restore the saved configuration only if no subsequent edits exist.
    Rollback {
        #[arg(long)]
        apply: bool,
    },
}

fn literal(path: &Path) -> Result<&str> {
    let text = path.to_str().context("MCP paths must be UTF-8")?;
    ensure!(
        !text.contains("${"),
        "MCP paths must not contain client environment expansion syntax"
    );
    Ok(text)
}

fn desired(root: &Path, socket: &Path) -> Result<Value> {
    let executable = std::env::current_exe()?.canonicalize()?;
    ensure!(
        executable.file_name().is_some_and(|name| name == "forge"),
        "MCP setup must be run by a native executable named forge"
    );
    Ok(json!({"type":"stdio", "command":literal(&executable)?,
        "args":["harness","stdio","--socket",literal(socket)?,"--source-root",literal(root)?]}))
}

fn parse(state: &Option<FileState>) -> Result<Value> {
    let config = match state {
        Some(state) => forge::tool_schema::parse_document(&state.text)?,
        None => json!({}),
    };
    ensure!(config.is_object(), "MCP configuration must be an object");
    ensure!(
        config.get("mcpServers").is_none_or(Value::is_object),
        "mcpServers must be an object"
    );
    Ok(config)
}

fn native(entry: &Value, root: &Path) -> bool {
    let Some(object) = entry.as_object() else {
        return false;
    };
    let Some(command) = entry["command"].as_str() else {
        return false;
    };
    let Some(args) = entry["args"].as_array() else {
        return false;
    };
    object.len() == 3
        && entry["type"] == "stdio"
        && Path::new(command).is_absolute()
        && Path::new(command)
            .file_name()
            .is_some_and(|name| name == "forge")
        && args.len() == 6
        && args[0] == "harness"
        && args[1] == "stdio"
        && args[2] == "--socket"
        && args[3].as_str().is_some_and(|p| Path::new(p).is_absolute())
        && args[4] == "--source-root"
        && args[5].as_str().map(Path::new) == Some(root)
}

fn legacy(entry: &Value, root: &Path) -> bool {
    let Some(command) = entry["command"].as_str() else {
        return false;
    };
    let Some(name) = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return false;
    };
    let python = name.strip_prefix("python").is_some_and(|version| {
        version.is_empty()
            || (version.starts_with('3')
                && version.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
    });
    python
        && entry.as_object().is_some_and(|object| object.len() == 5)
        && entry["type"] == "stdio"
        && entry["cwd"].as_str().map(Path::new) == Some(root)
        && entry["env"] == json!({"CRG_ROLE":"review"})
        && entry["args"] == json!(["-m", "conductor.crg_server", "--repo", root])
}

fn merge(
    before: &Option<FileState>,
    root: &Path,
    wanted: Option<&Value>,
    replace_legacy: bool,
) -> Result<Option<FileState>> {
    let original = parse(before)?;
    let mut config = original.clone();
    if config.get("mcpServers").is_none() {
        if wanted.is_none() {
            return Ok(before.clone());
        }
        config["mcpServers"] = json!({});
    }
    let servers = config["mcpServers"]
        .as_object_mut()
        .context("MCP server map")?;
    if let Some(entry) = servers.get("forge") {
        ensure!(
            native(entry, root),
            "MCP server 'forge' is customized or belongs to another host; refusing to overwrite it"
        );
    }
    if wanted.is_some()
        && servers
            .get("code-review-graph")
            .is_some_and(|entry| legacy(entry, root))
    {
        ensure!(
            replace_legacy,
            "recognized interpreted graph server remains; use --replace-legacy to migrate it"
        );
        servers.remove("code-review-graph");
    }
    if let Some(wanted) = wanted {
        servers.insert("forge".into(), wanted.clone());
    } else {
        servers.remove("forge");
    }
    if config == original {
        return Ok(before.clone());
    }
    let text = format!("{}\n", serde_json::to_string_pretty(&config)?);
    ensure!(
        text.len() <= store::CONFIG_LIMIT,
        "rendered MCP configuration exceeds 1 MiB"
    );
    Ok(Some(FileState {
        text,
        mode: before.as_ref().map_or(0o600, |s| s.mode),
    }))
}

fn socket_identity(socket: &Path) -> Result<(u64, u64)> {
    let metadata = fs::symlink_metadata(socket).context("inspect harness socket")?;
    ensure!(
        metadata.file_type().is_socket()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
            && metadata.nlink() == 1,
        "harness socket must be private, owned, nonsymlink and singly linked"
    );
    Ok((metadata.dev(), metadata.ino()))
}

fn socket_path(path: &Path) -> Result<PathBuf> {
    socket_identity(path)?;
    let canonical = path.canonicalize()?;
    literal(&canonical)?;
    // The service's private parent prevents other UIDs replacing the endpoint.
    let parent = canonical.parent().context("harness socket parent")?;
    let meta = fs::symlink_metadata(parent)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "harness socket parent must be private and owned"
    );
    Ok(canonical)
}

fn probe(root: &Path, socket: &Path) -> Result<Value> {
    let identity = socket_identity(socket)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(3), async {
            let status = transport::verify_host(socket, root).await?;
            let catalog = transport::request(socket, br#"{"method":"catalog"}"#).await?;
            ensure!(catalog["ok"] == true, "harness catalog failed");
            forge::tool_schema::ToolRegistry::from_json(&serde_json::to_string(
                &catalog["result"],
            )?)?;
            let tools: Vec<_> = catalog["result"]["tools"]
                .as_array()
                .context("missing tool catalog")?
                .iter()
                .map(|tool| tool["name"].clone())
                .collect();
            ensure!(
                socket_identity(socket)? == identity,
                "harness socket changed while probing; retry"
            );
            Ok::<_, anyhow::Error>(json!({"status":status, "tools":tools}))
        })
        .await
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    result.context("harness binding probe deadline exceeded")?
}

fn summary(state: &Option<FileState>) -> Value {
    state.as_ref().map_or(Value::Null, |state| {
        json!({
        "sha256":format!("{:x}", Sha256::digest(state.text.as_bytes())),
        "bytes":state.text.len(), "mode":state.mode})
    })
}

pub(crate) fn run(args: McpArgs) -> Result<u8> {
    let root = args.host.canonicalize()?;
    let relative = Path::new(".mcp.json");
    store::parents(&root, relative, false)?;
    let path = root.join(relative);
    let before = store::read(&path)?;
    parse(&before)?;
    let mut wanted = None;
    let mut service = Value::Null;
    let mut bound_socket = None;
    let (after, apply, rollback) = match args.action {
        Action::Bind {
            socket,
            apply,
            replace_legacy,
        } => {
            let socket = socket_path(&socket)?;
            let entry = desired(&root, &socket)?;
            let after = merge(&before, &root, Some(&entry), replace_legacy)?;
            service = probe(&root, &socket)?;
            bound_socket = Some(socket);
            wanted = Some(entry);
            (after, apply, false)
        }
        Action::Check { socket } => {
            let socket = socket_path(&socket)?;
            let entry = desired(&root, &socket)?;
            ensure!(
                merge(&before, &root, Some(&entry), false)? == before,
                "installed MCP entry differs from requested binding"
            );
            println!(
                "{}",
                json!({"schema":"forge.mcp-check.v1", "configured":true,
                "path":path, "service":probe(&root,&socket)?, "services_started":false})
            );
            return Ok(0);
        }
        Action::Unbind { apply } => (merge(&before, &root, None, false)?, apply, false),
        Action::Rollback { apply } => {
            let backup = store::read_backup(&path)?;
            ensure!(
                before == backup.after || before == backup.before,
                "MCP settings changed after the saved operation; refusing rollback"
            );
            parse(&backup.before)?;
            (backup.before, apply, true)
        }
    };
    if apply && before != after {
        let _lock = store::lock_named(&path, "mcp")?;
        ensure!(
            store::read(&path)? == before,
            "MCP settings changed since planning; retry"
        );
        if rollback {
            let backup = store::read_backup(&path)?;
            ensure!(
                backup.before == after && backup.after == before,
                "MCP backup changed since planning; retry"
            );
            store::restore(&path, &after)?;
        } else {
            if let Some(socket) = &bound_socket {
                service = probe(&root, socket)?;
            }
            store::apply(&path, &before, &after)?;
        }
    }
    println!(
        "{}",
        json!({"schema":"forge.mcp-binding.v1", "path":path,
        "before":summary(&before), "after":summary(&after), "changed":before != after,
        "applied":apply, "desired_server":wanted, "service":service, "services_started":false})
    );
    Ok(0)
}
