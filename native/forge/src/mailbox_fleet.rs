//! Read-only bounded coordination view. Observations are never authorization.
#[path = "mailbox_fleet_processes.rs"]
mod processes;
use super::{registry, store::Store, view};
use anyhow::{ensure, Context, Result};
use clap::Args;
use conductor_native::fleet_status;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct FleetArgs {
    /// Read summaries from this registered inbox; never acknowledge messages.
    #[arg(long)]
    as_name: String,
    #[arg(long)]
    json: bool,
    /// Include bounded same-user /proc observations matched to configured paths.
    #[arg(long)]
    processes: bool,
}

fn public_peer(observed: &Value) -> Value {
    let mut public = json!({"name":observed["name"],"port":observed["port"],
        "status":observed["status"]});
    for key in ["reason", "version"] {
        if let Some(text) = observed[key].as_str() {
            public[key] = json!(text.chars().take(200).collect::<String>());
        }
    }
    public
}

fn peer_observations(root: &Path) -> Result<Map<String, Value>> {
    let records = registry::load(root)?;
    ensure!(
        records.len() <= 512,
        "fleet registry exceeds 512 identities"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut peers = Map::new();
    for (name, record) in records {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let observation = if remaining.is_zero() {
            json!({"name":name,"port":record.port,"status":"unprobed",
                "reason":"fleet probe time budget exhausted"})
        } else {
            registry::probe_for(&record, remaining.min(Duration::from_millis(200)))
        };
        peers.insert(name, public_peer(&observation));
    }
    Ok(peers)
}

fn last_heard(preview: &Value) -> Result<Map<String, Value>> {
    let mut heard: Map<String, Value> = Map::new();
    for message in preview["messages"]
        .as_array()
        .context("missing inbox summaries")?
    {
        let sender = message["from"].as_str().context("missing summary sender")?;
        let at = message["at"]
            .as_str()
            .context("missing summary timestamp")?;
        let stamp = forge::instant::parse(at).context("invalid summary timestamp")?;
        let summary = message["summary"]
            .as_str()
            .context("missing message summary")?;
        let previous = heard
            .get(sender)
            .and_then(|row| row["at"].as_str())
            .and_then(forge::instant::parse);
        if previous.is_none_or(|previous| stamp > previous) {
            heard.insert(
                sender.to_owned(),
                json!({"at":at,
                "said":summary.chars().take(160).collect::<String>()}),
            );
        }
    }
    Ok(heard)
}

pub fn run(args: FleetArgs, host: &Path, root: &Path) -> Result<()> {
    let host = host.canonicalize().context("fleet host directory")?;
    let store = Store::required(root, &args.as_name, false)?;
    let preview = view::inbox(&store, &args.as_name, false, 8, 160, 10_000)?;
    let heard = last_heard(&preview)?;
    let state = serde_json::to_value(forge::active_state::generate_active_state(&host)?)?;
    let peers = peer_observations(root)?;
    let processes = if args.processes {
        processes::capture(&host)?
    } else {
        processes::Observation {
            groups: Map::new(),
            coverage: json!({"scope":"not requested"}),
        }
    };
    let mut report = fleet_status::build_report(
        &peers,
        &heard,
        state.as_object().context("invalid active state")?,
        &processes.groups,
        &forge::instant::isoformat_utc(forge::instant::now()),
        host.to_str().context("fleet host must be UTF-8")?,
    );
    report["schema_version"] = json!(1);
    report["authority"] = json!("bounded-fleet-observation");
    report["inbox"] = json!({"identity":args.as_name,"shown":preview["shown"],
        "total":preview["total"],"omitted":preview["omitted"],
        "scope":"latest per sender among at most eight selected compact summaries"});
    report["peer_observations"] = json!(peers);
    report["processes_requested"] = json!(args.processes);
    report["process_coverage"] = processes.coverage;
    println!("{}", report_output(&report, args.json)?);
    Ok(())
}

fn report_output(report: &Value, as_json: bool) -> Result<String> {
    const MAX_OUTPUT: usize = 512 * 1024;
    let json = serde_json::to_string(report)?;
    ensure!(
        json.len() <= MAX_OUTPUT,
        "fleet report exceeds 512 KiB; narrow host inputs"
    );
    let output = if as_json {
        json
    } else {
        let unprobed = report["peer_observations"]
            .as_object()
            .context("peer observations")?
            .values()
            .filter(|peer| peer["status"] == "unprobed")
            .count();
        format!("{}\n\nInbox: {} shown, {} omitted. Last heard means latest per sender among at most eight selected summaries; no messages acknowledged.\nPeers: {unprobed} unprobed. Process coverage: {}",
            fleet_status::render(report.as_object().context("fleet report")?)
                .map_err(anyhow::Error::msg)?, report["inbox"]["shown"],
            report["inbox"]["omitted"], report["process_coverage"])
    };
    ensure!(
        output.len() <= MAX_OUTPUT,
        "fleet text report exceeds 512 KiB"
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latest_summary_compares_instants_and_truncates_unicode() {
        let preview = json!({"messages":[
            {"from":"one","at":"2026-10-31T03:00:00+03:00","summary":"old"},
            {"from":"one","at":"2026-10-31T01:00:00+00:00","summary":"λ".repeat(200)}]});
        let heard = last_heard(&preview).unwrap();
        assert_eq!(heard["one"]["said"], "λ".repeat(160));
        assert!(last_heard(&json!({"messages":[{"from":"one","at":"bad"}]})).is_err());
    }

    #[test]
    fn peer_cards_cannot_expand_report_with_arbitrary_fields() {
        let card = json!({"name":"peer","port":1234,"status":"up",
            "version":"x".repeat(1000),"skills":[{"arbitrary":"x".repeat(100_000)}]});
        let public = public_peer(&card);
        assert_eq!(public["version"].as_str().unwrap().len(), 200);
        assert!(public.get("skills").is_none());
        assert!(serde_json::to_vec(&public).unwrap().len() < 300);
        assert!(report_output(&json!({"oversize":"x".repeat(512 * 1024)}), true).is_err());
    }
}
