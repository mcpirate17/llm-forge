//! Native local A2A delivery over the existing JSON-RPC wire protocol.

use super::queue::{self, EnqueueArgs, SenderLock, ValidatedMessage};
use super::store::{self, PendingOutbound, Store};
use anyhow::{bail, ensure, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const CARD_BYTES: u64 = 65_536;
const REPLY_BYTES: u64 = 65_536;
const MAX_REGISTRY_BYTES: u64 = 1_048_576;
const CARD_TIMEOUT: Duration = Duration::from_secs(1);
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Args)]
pub struct SendArgs {
    #[command(flatten)]
    message: EnqueueArgs,
    /// Fail terminally if this message cannot reach its peer.
    #[arg(long)]
    no_queue: bool,
}

#[derive(Args)]
pub struct FlushArgs {
    /// Flush one sender, or every registered sender with an existing store.
    #[arg(long)]
    as_name: Option<String>,
    #[arg(long)]
    to: Option<String>,
    #[arg(long, default_value_t = 100)]
    max_messages: usize,
}

struct Peer {
    port: u16,
    token: String,
}

impl Peer {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

enum WireError {
    Unreachable(String),
    Rejected(String),
}

fn registry(root: &Path) -> Result<BTreeMap<String, Peer>> {
    let path = root.join("agents.json");
    let mut bytes = Vec::new();
    File::open(&path)
        .with_context(|| format!("reading A2A registry {}", path.display()))?
        .take(MAX_REGISTRY_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_REGISTRY_BYTES,
        "A2A registry exceeds 1 MiB"
    );
    let value: Value = serde_json::from_slice(&bytes).context("invalid A2A registry JSON")?;
    ensure!(
        value["schema_version"].as_u64() == Some(1),
        "unsupported A2A registry schema"
    );
    let agents = value["agents"]
        .as_object()
        .context("registry agents must be an object")?;
    ensure!(!agents.is_empty(), "A2A registry lists no agents");
    let mut peers = BTreeMap::new();
    for (name, row) in agents {
        store::validate_identity(name)?;
        let port = row["port"]
            .as_u64()
            .filter(|port| (1..=65_535).contains(port))
            .with_context(|| format!("agent {name:?} has no usable port"))?
            as u16;
        let token = row["token"]
            .as_str()
            .filter(|token| token.chars().count() >= 16)
            .with_context(|| format!("agent {name:?} has no usable token"))?;
        // Header parsing must not be affected by malformed registry content.
        ensure!(
            !token.chars().any(|ch| ch.is_control()),
            "agent {name:?} has an invalid token"
        );
        peers.insert(
            name.clone(),
            Peer {
                port,
                token: token.to_owned(),
            },
        );
    }
    Ok(peers)
}

fn client(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .proxy(None)
        .max_response_header_size(16_384)
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn response_json(
    mut response: ureq::http::Response<ureq::Body>,
    limit: u64,
) -> std::result::Result<Value, WireError> {
    let bytes = response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|error| match error {
            ureq::Error::BodyExceedsLimit(_) => {
                WireError::Rejected(format!("peer response exceeds {limit} bytes"))
            }
            other => WireError::Unreachable(other.to_string()),
        })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| WireError::Rejected(format!("invalid peer JSON: {error}")))
}

fn card(peer_name: &str, peer: &Peer) -> std::result::Result<Value, WireError> {
    let response = client(CARD_TIMEOUT)
        .get(peer.url("/.well-known/agent-card.json"))
        .call()
        .map_err(|error| WireError::Unreachable(error.to_string()))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(WireError::Rejected(format!(
            "card fetch returned HTTP {status}"
        )));
    }
    let card = response_json(response, CARD_BYTES)?;
    if card["name"].as_str() != Some(peer_name) {
        return Err(WireError::Rejected(format!(
            "card name {:?} does not match registry {peer_name:?}",
            card["name"]
        )));
    }
    Ok(card)
}

fn require_v2(peer_name: &str, card: &Value) -> std::result::Result<(), WireError> {
    let advertised = card["skills"]
        .as_array()
        .is_some_and(|skills| skills.iter().any(|skill| skill["id"] == "coordination-v2"));
    if advertised {
        Ok(())
    } else {
        Err(WireError::Rejected(format!(
            "peer {peer_name:?} does not advertise coordination-v2; refusing structured send"
        )))
    }
}

fn data_value(data_json: Option<&str>) -> std::result::Result<Option<Value>, WireError> {
    let Some(data_json) = data_json else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(data_json)
        .map_err(|error| WireError::Rejected(format!("invalid stored data_json: {error}")))?;
    if !value.is_object() {
        return Err(WireError::Rejected(
            "stored data_json must be an object".into(),
        ));
    }
    Ok(Some(value))
}

fn deliver(
    peer_name: &str,
    peer: &Peer,
    row: &PendingOutbound,
    known_card: Option<Value>,
) -> std::result::Result<(), WireError> {
    let data = data_value(row.data_json.as_deref())?;
    let fetched = match known_card {
        Some(card) => card,
        None => card(peer_name, peer)?,
    };
    if data
        .as_ref()
        .is_some_and(|data| data["kind"] == "coordination-v2")
    {
        require_v2(peer_name, &fetched)?;
    }
    let mut parts = vec![json!({"text":row.body})];
    if let Some(data) = data {
        parts.push(json!({"data":data}));
    }
    let request = json!({"jsonrpc":"2.0","id":row.id,"method":"SendMessage",
        "params":{"message":{"messageId":row.id,"role":"ROLE_USER",
        "parts":parts,"metadata":{"sender":row.sender}}}});
    let encoded = serde_json::to_vec(&request)
        .map_err(|error| WireError::Rejected(format!("cannot serialize message: {error}")))?;
    let response = client(SEND_TIMEOUT)
        .post(peer.url("/"))
        .header("X-A2A-Token", peer.token.as_str())
        .header("A2A-Version", "1.0")
        .header("Content-Type", "application/json")
        .send(encoded.as_slice())
        .map_err(|error| WireError::Unreachable(error.to_string()))?;
    let status = response.status().as_u16();
    let payload = response_json(response, REPLY_BYTES)?;
    if status != 200 || payload.get("error").is_some() {
        let reason = payload["error"]["message"]
            .as_str()
            .unwrap_or("peer rejected request");
        return Err(WireError::Rejected(format!(
            "peer rejected message: {reason}"
        )));
    }
    let ack = payload["result"]["message"]["parts"]
        .as_array()
        .and_then(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("data"))
                .find(|data| data["kind"] == "delivery-receipt")
        });
    if ack.and_then(|data| data["message_id"].as_str()) != Some(row.id.as_str()) {
        return Err(WireError::Rejected(
            "peer ack did not echo the message_id".into(),
        ));
    }
    Ok(())
}

fn attempt(
    store: &mut Store,
    peer_name: &str,
    peer: &Peer,
    row: &PendingOutbound,
    known_card: Option<Value>,
) -> Result<&'static str> {
    let (status, reason) = match deliver(peer_name, peer, row, known_card) {
        Ok(()) => ("delivered", None),
        Err(WireError::Unreachable(error)) => {
            ("queued", Some(format!("peer unreachable: {error}")))
        }
        Err(WireError::Rejected(error)) => (
            "failed",
            Some(format!(
                "peer rejected or returned an invalid response: {error}"
            )),
        ),
    };
    let truncated = reason
        .as_deref()
        .map(|reason| reason.chars().take(500).collect::<String>());
    store.mark_outbound(&row.id, status, truncated.as_deref())?;
    Ok(status)
}

fn flush_store(
    store: &mut Store,
    records: &BTreeMap<String, Peer>,
    to: Option<&str>,
    limit: usize,
) -> Result<Vec<Value>> {
    let mut output = Vec::new();
    let mut unreachable = BTreeSet::new();
    while output.len() < limit {
        let excluded = unreachable.iter().cloned().collect::<Vec<_>>();
        let Some(row) = store.next_outbound(to, &excluded)? else {
            break;
        };
        let status = if let Some(peer) = records.get(&row.recipient) {
            attempt(store, &row.recipient, peer, &row, None)?
        } else {
            store.mark_outbound(&row.id, "failed", Some("recipient no longer registered"))?;
            "failed"
        };
        if status == "queued" {
            unreachable.insert(row.recipient.clone());
        }
        output.push(json!({"message_id":row.id,"sender":row.sender,"recipient":row.recipient,"status":status}));
    }
    Ok(output)
}

fn preflight_v2(name: &str, peer: &Peer, input: &ValidatedMessage) -> Result<Option<Value>> {
    if !input.data_json.as_deref().is_some_and(|data| {
        serde_json::from_str::<Value>(data).is_ok_and(|value| value["kind"] == "coordination-v2")
    }) {
        return Ok(None);
    }
    match card(name, peer) {
        Ok(card) => {
            require_v2(name, &card).map_err(wire_as_error)?;
            Ok(Some(card))
        }
        Err(WireError::Unreachable(_)) => Ok(None),
        Err(error) => Err(wire_as_error(error)),
    }
}

fn wire_as_error(error: WireError) -> anyhow::Error {
    match error {
        WireError::Unreachable(reason) | WireError::Rejected(reason) => anyhow::anyhow!(reason),
    }
}

pub fn send(args: SendArgs, state_dir: &Path) -> Result<(Value, u8)> {
    let records = registry(state_dir)?;
    let input = queue::validate_input(args.message)?;
    let peer = records.get(&input.recipient).with_context(|| {
        format!(
            "unknown recipient {:?}; registered: {:?}",
            input.recipient,
            records.keys().collect::<Vec<_>>()
        )
    })?;
    ensure!(
        records.contains_key(&input.sender),
        "unknown sender {:?}",
        input.sender
    );
    let preflight_card = preflight_v2(&input.recipient, peer, &input)?;
    let mut store = Store::required(state_dir, &input.sender, true)?;
    store.require_enqueue_schema()?;
    let _lock = SenderLock::acquire(state_dir, &input.sender)?;
    flush_store(&mut store, &records, Some(&input.recipient), 100)?;
    let waiting = store.has_outbound(Some(&input.recipient))?;
    let prepared = queue::prepare(input)?;
    store.enqueue(&prepared, "awaiting explicit flush")?;
    if waiting {
        store.mark_outbound(
            &prepared.id,
            "queued",
            Some("waiting for earlier outbound messages"),
        )?;
    } else {
        let row = store
            .next_outbound(Some(&prepared.recipient), &[])?
            .context("newly enqueued message is not retryable")?;
        ensure!(
            row.id == prepared.id,
            "new message overtook earlier outbound delivery"
        );
        attempt(&mut store, &prepared.recipient, peer, &row, preflight_card)?;
    }
    let mut receipt = store.outbound_receipt(&prepared.id)?;
    if receipt["delivery_status"] == "queued" && args.no_queue {
        let reason = format!(
            "{}; queue disabled",
            receipt["status_reason"]
                .as_str()
                .unwrap_or("peer unreachable")
        );
        store.mark_outbound(&prepared.id, "failed", Some(&reason))?;
        bail!(reason);
    }
    if receipt["delivery_status"] == "failed" {
        bail!(
            "{}",
            receipt["status_reason"]
                .as_str()
                .unwrap_or("peer rejected message")
        );
    }
    let exit = if receipt["delivery_status"] == "delivered" {
        0
    } else {
        3
    };
    receipt["schema_version"] = json!(1);
    Ok((receipt, exit))
}

pub fn flush(args: FlushArgs, state_dir: &Path) -> Result<(Value, u8)> {
    ensure!(
        (1..=1000).contains(&args.max_messages),
        "max_messages must be between 1 and 1000"
    );
    let records = registry(state_dir)?;
    if let Some(name) = &args.as_name {
        ensure!(records.contains_key(name), "unknown sender {name:?}");
    }
    let senders = match args.as_name {
        Some(name) => vec![name],
        None => records
            .keys()
            .filter(|name| state_dir.join(name).join("store.sqlite").is_file())
            .cloned()
            .collect(),
    };
    let mut output = Vec::new();
    let mut remaining = false;
    for sender in senders {
        let mut store = Store::required(state_dir, &sender, true)?;
        store.require_enqueue_schema()?;
        let _lock = SenderLock::acquire(state_dir, &sender)?;
        if output.len() < args.max_messages {
            output.extend(flush_store(
                &mut store,
                &records,
                args.to.as_deref(),
                args.max_messages - output.len(),
            )?);
        }
        remaining |= store.has_outbound(args.to.as_deref())?;
    }
    Ok((Value::Array(output), if remaining { 3 } else { 0 }))
}
