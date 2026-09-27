//! Read-only local platform health. Missing, broken, and unchecked evidence are
//! distinct states; reading status never creates a database or consumes inboxes.

use crate::{active_state, ledger, task_store, tasks};
use anyhow::{ensure, Context, Result};
use clap::Args;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

#[derive(Args)]
pub struct StatusArgs {
    #[arg(long, default_value = ".")]
    host: PathBuf,
    #[arg(long)]
    state_dir: Option<PathBuf>,
    #[arg(long)]
    ledger_root: Option<PathBuf>,
    #[arg(long, default_value_t = 100)]
    limit: usize,
    /// Hash task-linked receipts; ordinary status only checks their existence.
    #[arg(long)]
    verify_receipts: bool,
    /// Exit nonzero for degraded health as well as unreadable state.
    #[arg(long)]
    check: bool,
    #[arg(long)]
    json: bool,
}

fn read_json(path: &Path) -> Result<Value> {
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&raw).with_context(|| format!("parsing {}", path.display()))
}

fn task_health(
    host: &Path,
    claims: &[active_state::ActiveClaim],
    args: &StatusArgs,
) -> Result<Value> {
    let Some(store) = task_store::Store::open(host, false)? else {
        return Ok(json!({"status":"not_initialized","total":0,"counts":{},"tasks":[]}));
    };
    let rows = store.list(args.limit)?;
    let counts = store.counts()?;
    let total: i64 = counts.values().sum();
    let mut links = Vec::with_capacity(rows.len());
    for task in rows {
        let claim = task.claim_id.as_ref().map(|id| {
            claims
                .iter()
                .any(|claim| claim.claim_id == *id && Some(&claim.owner) == task.owner.as_ref())
        });
        let receipt = match &task.receipt {
            None => "not_recorded",
            Some(receipt) if !receipt.path.is_file() => "missing",
            Some(receipt) if args.verify_receipts => {
                if tasks::bind_receipt(host, &receipt.path)?.sha256 == receipt.sha256 {
                    "verified"
                } else {
                    "changed"
                }
            }
            Some(_) => "unchecked",
        };
        let expired = task.state == task_store::State::Running
            && task.lease_until.is_some_and(|t| t <= tasks::now());
        links.push(json!({"task":task,"claim_active":claim,"receipt_status":receipt,"lease_expired":expired}));
    }
    Ok(
        json!({"status":"available","database":task_store::path(host),"counts":counts,
        "total":total,"truncated":total > links.len() as i64,"tasks":links,
        "expired_leases":store.expired_count(tasks::now())?}),
    )
}

fn safe_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn mailbox(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({"status":"not_initialized"}));
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_secs(1))?;
    let mut statement = connection.prepare(
        "SELECT delivery_status,count(*) FROM messages WHERE direction='outbound' GROUP BY delivery_status")?;
    let statuses: BTreeMap<String, i64> = statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let unread: i64 = connection.query_row(
        "SELECT count(*) FROM messages WHERE direction='inbound' AND read_at IS NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(json!({"status":"available","outbound":statuses,"inbound_unread":unread}))
}

fn process_alive(pid: u64) -> bool {
    if pid == 0 || pid > i32::MAX as u64 {
        return false;
    }
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, 0) == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn supervisor(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({"status":"not_configured"}));
    }
    let value = read_json(path)?;
    let pid = value.get("pid").and_then(Value::as_u64);
    let alive = pid.is_some_and(process_alive);
    Ok(
        json!({"status":value.get("status"),"pid":pid,"process_alive":alive,
        "updated_at":value.get("updated_at"),"cycles":value.get("cycles"),
        "endpoint_pid":value.get("endpoint_pid")}),
    )
}

fn messaging(root: &Path, errors: &mut Vec<String>) -> Result<Value> {
    let path = root.join("agents.json");
    if !path.exists() {
        return Ok(json!({"status":"not_initialized","agents":[]}));
    }
    let registry = read_json(&path)?;
    let agents = registry
        .get("agents")
        .and_then(Value::as_object)
        .context("A2A registry requires agents object")?;
    let mut rows = Vec::new();
    for (name, record) in agents {
        ensure!(safe_name(name), "unsafe agent name in registry: {name:?}");
        let directory = root.join(name);
        let inbox = capture(
            &format!("mailbox {name}"),
            mailbox(&directory.join("store.sqlite")),
            errors,
        );
        let supervisor = capture(
            &format!("supervisor {name}"),
            supervisor(&directory.join("supervisor.json")),
            errors,
        );
        // Tokens and message bodies deliberately never enter this report.
        rows.push(
            json!({"name":name,"port":record.get("port"),"mailbox":inbox,"supervisor":supervisor}),
        );
    }
    Ok(json!({"status":"available","state_dir":root,"agents":rows}))
}

fn hook_configs(host: &Path) -> Result<Value> {
    let mut configs = Vec::new();
    for (provider, relative) in [
        ("claude", ".claude/settings.json"),
        ("codex", ".codex/hooks.json"),
        ("qwen", ".qwen/settings.json"),
        ("grok", ".grok/settings.json"),
    ] {
        let path = host.join(relative);
        if !path.exists() {
            configs.push(json!({"provider":provider,"status":"not_configured"}));
            continue;
        }
        let value = read_json(&path)?;
        let hooks = value.get("hooks").and_then(Value::as_object);
        configs.push(
            json!({"provider":provider,"status":"configured_unverified","path":path,
            "events":hooks.map(|map| map.keys().collect::<Vec<_>>()).unwrap_or_default()}),
        );
    }
    Ok(Value::Array(configs))
}

fn capture(name: &str, result: Result<Value>, errors: &mut Vec<String>) -> Value {
    match result {
        Ok(value) => value,
        Err(error) => {
            let detail = format!("{name}: {error:#}");
            errors.push(detail.clone());
            json!({"status":"error","detail":detail})
        }
    }
}

fn ledger_sessions(root: &Path, host: &Path, tasks: &Value) -> Result<Value> {
    let wanted: BTreeSet<&str> = tasks["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["task"]["session_id"].as_str())
        .collect();
    let table = root.join("session_rollup");
    if !table.is_dir() {
        return Ok(
            json!({"root":root,"exists":root.is_dir(),"status":"not_initialized","sessions":[]}),
        );
    }
    if wanted.is_empty() {
        return Ok(json!({"root":root,"exists":true,"status":"no_linked_sessions","sessions":[]}));
    }
    let mut files = std::fs::read_dir(&table)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    files.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "jsonl")
    });
    files.sort_unstable_by(|a, b| b.cmp(a));
    let mut truncated = files.len() > 31;
    let mut budget = 8 * 1024 * 1024;
    let mut sessions = BTreeMap::new();
    for file in files.into_iter().take(31) {
        let mut raw = Vec::new();
        std::fs::File::open(&file)?
            .take(budget + 1)
            .read_to_end(&mut raw)?;
        if raw.len() as u64 > budget {
            truncated = true;
            break;
        }
        budget -= raw.len() as u64;
        for line in raw
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let row: Value = serde_json::from_slice(line)
                .with_context(|| format!("reading ledger {}", file.display()))?;
            let id = row["session_id"]
                .as_str()
                .context("session rollup row has no session_id")?;
            if !wanted.contains(id)
                || row["project"].as_str() != Some(host.to_string_lossy().as_ref())
            {
                continue;
            }
            sessions.entry(id.to_owned()).or_insert_with(|| json!({"session_id":id,
                "total_input":row.get("total_input"),"total_output":row.get("total_output"),
                "total_cache_read":row.get("total_cache_read"),"total_cache_creation":row.get("total_cache_creation"),
                "last_ts":row.get("last_ts"),"source":file}));
        }
    }
    let unmatched: Vec<_> = wanted
        .into_iter()
        .filter(|id| !sessions.contains_key(*id))
        .collect();
    Ok(
        json!({"root":root,"exists":true,"status":"bounded_snapshot","sessions":sessions.values().collect::<Vec<_>>(),
        "unmatched_session_ids":unmatched,"truncated":truncated,"day_file_limit":31,"byte_limit":8*1024*1024,
        "attribution":"session totals; shared sessions must not be summed once per task"}),
    )
}

fn warnings(tasks: &Value, messaging: &Value) -> Vec<String> {
    let mut warnings = Vec::new();
    if tasks
        .get("expired_leases")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        > 0
    {
        warnings.push("tasks have expired leases; inspect and explicitly retry".into());
    }
    if let Some(rows) = tasks.get("tasks").and_then(Value::as_array) {
        for row in rows {
            let task = &row["task"];
            let id = task["id"].as_str().unwrap_or("unknown");
            if ["assigned", "running"].contains(&task["state"].as_str().unwrap_or(""))
                && row["claim_active"] == false
            {
                warnings.push(format!("task {id} refers to an inactive claim"));
            }
            if ["missing", "changed"].contains(&row["receipt_status"].as_str().unwrap_or("")) {
                warnings.push(format!("task {id} receipt is {}", row["receipt_status"]));
            }
        }
    }
    if let Some(agents) = messaging.get("agents").and_then(Value::as_array) {
        for agent in agents {
            let queued = agent["mailbox"]["outbound"]["queued"].as_i64().unwrap_or(0)
                + agent["mailbox"]["outbound"]["pending"]
                    .as_i64()
                    .unwrap_or(0);
            if queued > 0 && agent["supervisor"]["process_alive"] != true {
                warnings.push(format!(
                    "agent {} has {queued} pending messages without a live supervisor",
                    agent["name"]
                ));
            }
        }
    }
    warnings
}

pub fn run(args: StatusArgs) -> Result<u8> {
    ensure!((1..=1000).contains(&args.limit), "limit must be 1..1000");
    let host = args.host.canonicalize().context("resolving --host")?;
    let mut errors = Vec::new();
    let claims = match active_state::parse_active_claims(&host) {
        Ok(claims) => claims,
        Err(error) => {
            errors.push(format!("claims: {error:#}"));
            Vec::new()
        }
    };
    let tasks = capture("tasks", task_health(&host, &claims, &args), &mut errors);
    let root = args
        .state_dir
        .clone()
        .unwrap_or_else(|| host.join(".agents/a2a"));
    let message_result = messaging(&root, &mut errors);
    let messaging = capture("messaging", message_result, &mut errors);
    let hooks = capture("hooks", hook_configs(&host), &mut errors);
    let ledger_root = ledger::resolve_ledger_root(args.ledger_root.clone());
    let ledger = capture(
        "ledger",
        ledger_sessions(&ledger_root, &host, &tasks),
        &mut errors,
    );
    let warnings = warnings(&tasks, &messaging);
    let healthy = errors.is_empty() && warnings.is_empty();
    let report = json!({"schema_version":1,"host":host,"checked_at":tasks::now(),
        "status":if healthy {"ok"} else {"degraded"},"claims":claims,"tasks":tasks,
        "messaging":messaging,"hooks":hooks,"ledger":ledger,"warnings":warnings,"errors":errors});
    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        render(&report);
    }
    Ok(if !errors.is_empty() || (args.check && !healthy) {
        1
    } else {
        0
    })
}

fn render(report: &Value) {
    println!(
        "platform | {} | {}",
        report["status"].as_str().unwrap_or("error"),
        report["host"]
    );
    println!(
        "claims | {} active",
        report["claims"].as_array().map_or(0, Vec::len)
    );
    println!(
        "tasks | {} | counts={}",
        report["tasks"]["status"], report["tasks"]["counts"]
    );
    println!(
        "messaging | {} | {} agents",
        report["messaging"]["status"],
        report["messaging"]["agents"].as_array().map_or(0, Vec::len)
    );
    println!("hooks | configured only; run provider doctor to verify execution");
    println!(
        "ledger | {} | exists={}",
        report["ledger"]["root"], report["ledger"]["exists"]
    );
    for kind in ["warnings", "errors"] {
        for row in report[kind].as_array().into_iter().flatten() {
            println!("{kind} | {}", row.as_str().unwrap_or("invalid diagnostic"));
        }
    }
}
