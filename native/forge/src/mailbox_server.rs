//! Bounded loopback Agent Card and A2A JSON-RPC SendMessage endpoint.

use super::registry::{self, Record};
use super::store::Store;
#[path = "mailbox_server_io.rs"]
mod io;
use anyhow::{ensure, Context, Result};
use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Args;
use serde_json::{json, Value};
use std::fs::File;
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

const MAX_BODY_BYTES: usize = 262_144;
const MAX_REQUEST_BYTES: usize = 1_376_256;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_ACTIVE_REQUESTS: usize = 16;
const MAX_DB_WRITERS: usize = 2;

#[derive(Args)]
pub struct ServeArgs {
    #[arg(long = "as-name", alias = "name")]
    pub name: String,
    #[arg(long)]
    pub port: Option<u16>,
}

#[derive(Clone)]
struct ServerState {
    record: Record,
    state_dir: PathBuf,
    active: Arc<Semaphore>,
    writers: Arc<Semaphore>,
}

fn card(record: &Record) -> Value {
    json!({
        "name":record.name,
        "description":format!("Bounded local-agent coordination endpoint for {}",record.name),
        "version":"1.0.0",
        "capabilities":{"streaming":false,"pushNotifications":false},
        "defaultInputModes":["application/json"],
        "defaultOutputModes":["application/json"],
        "skills":[
            {"id":"coordination","name":"coordination","description":"Peer-to-peer status and handoff messages"},
            {"id":"coordination-v2","name":"coordination-v2","description":"Sender-authored bounded summaries, thread IDs, lifecycle status, and supersession edges for context-safe coordination"},
            {"id":"gate-review-request","name":"gate-review-request","description":"Structured nm_f6 gate review request (gate, fingerprint, artifact_paths); verdicts remain authoritative only via record_review receipts"}
        ],
        "supportedInterfaces":[{"url":format!("{}/",record.base_url()),
            "protocolBinding":"JSONRPC","protocolVersion":"1.0"}]
    })
}

async fn card_handler(State(state): State<ServerState>) -> impl IntoResponse {
    Json(card(&state.record))
}

fn token_equal(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (left, right) in a.iter().zip(b.iter()) {
        diff |= usize::from(left ^ right);
    }
    diff == 0
}

fn rpc_error(id: Value, status: StatusCode, code: i64, message: &str) -> Response {
    (
        status,
        Json(json!({"jsonrpc":"2.0","id":id,
        "error":{"code":code,"message":message}})),
    )
        .into_response()
}

fn random_message_id() -> Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

fn validate_data(data: &Value) -> Result<()> {
    let kind = data["kind"]
        .as_str()
        .context("data part must have a supported kind")?;
    match kind {
        "coordination" => Ok(()),
        "coordination-v2" => {
            conductor_native::validate_coordination_v2_value(data).map_err(anyhow::Error::msg)?;
            Ok(())
        }
        "gate-review-request" => {
            let gate = data["gate"].as_f64();
            ensure!(
                gate.is_some_and(
                    |gate| gate.fract() == 0.0 && [1.0, 2.0, 3.0, 4.0, 5.0, 7.0].contains(&gate)
                ),
                "gate-review-request requires integer gate in {{1, 2, 3, 4, 5, 7}}"
            );
            let fingerprint = data["fingerprint"].as_str();
            ensure!(
                fingerprint.is_some_and(|s| s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())),
                "gate-review-request requires 64-hex fingerprint"
            );
            ensure!(
                data["artifact_paths"]
                    .as_array()
                    .is_some_and(|paths| !paths.is_empty()
                        && paths
                            .iter()
                            .all(|path| path.as_str().is_some_and(|s| !s.is_empty()))),
                "gate-review-request requires non-empty artifact_paths"
            );
            Ok(())
        }
        _ => anyhow::bail!("unknown data kind {kind:?}"),
    }
}

struct Inbound {
    id: String,
    sender: String,
    body: String,
    data_json: Option<String>,
}

fn parse_inbound(request: &Value) -> Result<Inbound> {
    let message = request["params"]["message"]
        .as_object()
        .context("SendMessage requires a message")?;
    let id = message
        .get("messageId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .map(Ok)
        .unwrap_or_else(random_message_id)?;
    super::store::validate_message_id(&id)?;
    let parts = message
        .get("parts")
        .and_then(Value::as_array)
        .context("message parts must be an array")?;
    let mut text = Vec::new();
    let mut data_parts = Vec::new();
    for part in parts {
        let part = part.as_object().context("message part must be an object")?;
        if let Some(body) = part.get("text") {
            text.push(body.as_str().context("text part must be a string")?);
        }
        if let Some(data) = part.get("data") {
            ensure!(data.is_object(), "data part must be a JSON object");
            validate_data(data)?;
            data_parts.push(data);
        }
    }
    let body = text.join("\n");
    ensure!(
        body.len() <= MAX_BODY_BYTES,
        "body exceeds {MAX_BODY_BYTES} bytes"
    );
    let data_json = data_parts
        .first()
        .map(|data| {
            let mut sorted = (*data).clone();
            sorted.sort_all_objects();
            serde_json::to_string(&sorted)
        })
        .transpose()?;
    let sender = message
        .get("metadata")
        .and_then(|value| value.get("sender"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    ensure!(sender.len() <= 4096, "sender exceeds 4096 bytes");
    Ok(Inbound {
        id,
        sender,
        body,
        data_json,
    })
}

async fn process_rpc(state: &ServerState, request: Request<Body>) -> Response {
    let provided = request
        .headers()
        .get("X-A2A-Token")
        .map(|v| v.as_bytes())
        .unwrap_or_default();
    if !token_equal(provided, state.record.token.as_bytes()) {
        return rpc_error(
            Value::Null,
            StatusCode::UNAUTHORIZED,
            -32001,
            "invalid agent token",
        );
    }
    if request
        .headers()
        .get("A2A-Version")
        .and_then(|v| v.to_str().ok())
        != Some("1.0")
    {
        return rpc_error(
            Value::Null,
            StatusCode::BAD_REQUEST,
            -32600,
            "unsupported A2A protocol version",
        );
    }
    let bytes = match to_bytes(request.into_body(), MAX_REQUEST_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return rpc_error(
                Value::Null,
                StatusCode::PAYLOAD_TOO_LARGE,
                -32600,
                "request body exceeds limit",
            )
        }
    };
    let request: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return rpc_error(Value::Null, StatusCode::BAD_REQUEST, -32700, "invalid JSON"),
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    if request["jsonrpc"] != "2.0" {
        return rpc_error(
            id,
            StatusCode::BAD_REQUEST,
            -32600,
            "invalid JSON-RPC version",
        );
    }
    if request["method"] != "SendMessage" {
        return rpc_error(id, StatusCode::OK, -32601, "method not found");
    }
    let inbound = match parse_inbound(&request) {
        Ok(inbound) => inbound,
        Err(error) => return rpc_error(id, StatusCode::OK, -32602, &error.to_string()),
    };
    let permit = match state.writers.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => {
            return rpc_error(
                id,
                StatusCode::SERVICE_UNAVAILABLE,
                -32000,
                "store unavailable",
            )
        }
    };
    let directory = state.state_dir.clone();
    let recipient = state.record.name.clone();
    let message_id = inbound.id.clone();
    let persisted = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut store = Store::required(&directory, &recipient, true)?;
        store.record_inbound(
            &inbound.id,
            &inbound.sender,
            &recipient,
            &inbound.body,
            inbound.data_json.as_deref(),
        )
    })
    .await;
    match persisted {
        Ok(Ok(())) => {
            let response_id = random_message_id().unwrap_or_else(|_| message_id.clone());
            Json(json!({"jsonrpc":"2.0","id":id,"result":{"message":{
                "messageId":response_id,"role":"ROLE_AGENT",
                "parts":[{"data":{"kind":"delivery-receipt","message_id":message_id,
                    "recipient":state.record.name}}]}}}))
            .into_response()
        }
        Ok(Err(error)) => rpc_error(id, StatusCode::OK, -32603, &error.to_string()),
        Err(error) => rpc_error(
            id,
            StatusCode::INTERNAL_SERVER_ERROR,
            -32603,
            &error.to_string(),
        ),
    }
}

async fn rpc_handler(State(state): State<ServerState>, request: Request<Body>) -> Response {
    let permit = match state.active.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return rpc_error(
                Value::Null,
                StatusCode::SERVICE_UNAVAILABLE,
                -32000,
                "server busy",
            )
        }
    };
    let result = tokio::time::timeout(REQUEST_TIMEOUT, process_rpc(&state, request)).await;
    drop(permit);
    match result {
        Ok(response) => response,
        Err(_) => rpc_error(
            Value::Null,
            StatusCode::REQUEST_TIMEOUT,
            -32000,
            "request timed out",
        ),
    }
}

pub fn serve(args: ServeArgs, state_dir: PathBuf) -> Result<()> {
    let port = registry::serve_port(&state_dir, &args.name, args.port)?;
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
        .with_context(|| format!("cannot bind {:?} to 127.0.0.1:{port}", args.name))?;
    listener.set_nonblocking(true)?;
    Store::initialize(&state_dir, &args.name)?;
    let records = registry::init(&state_dir, Some(&args.name), Some(port), true)?;
    let record = records
        .get(&args.name)
        .context("new registration missing")?
        .clone();
    let state = ServerState {
        record: record.clone(),
        state_dir,
        active: Arc::new(Semaphore::new(MAX_ACTIVE_REQUESTS)),
        writers: Arc::new(Semaphore::new(MAX_DB_WRITERS)),
    };
    let app = Router::new()
        .route("/.well-known/agent-card.json", get(card_handler))
        .route("/", post(rpc_handler))
        .with_state(state);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(MAX_DB_WRITERS)
        .build()?;
    println!(
        "{}",
        json!({"name":record.name,"port":record.port,
        "generation":record.generation,"card_url":format!("{}/.well-known/agent-card.json",record.base_url()),
        "rpc_url":format!("{}/",record.base_url()),"pid":std::process::id()})
    );
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        axum::serve(io::BoundedListener::new(listener), app)
            .await
            .context("A2A HTTP server stopped")
    })
}
