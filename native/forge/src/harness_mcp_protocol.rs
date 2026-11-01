//! MCP 2026-07-28 request metadata and legacy initialization share one catalog.
//! Raw argument JSON survives framing; no Value roundtrip can erase duplicate keys.
use crate::tool_schema::ToolRegistry;
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, value::RawValue, Value};

const MODERN: &str = "2026-07-28";
const LEGACY: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];
const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    jsonrpc: String,
    #[serde(default, deserialize_with = "present_raw")]
    id: Option<Box<RawValue>>,
    method: String,
    #[serde(default, deserialize_with = "present_raw")]
    params: Option<Box<RawValue>>,
}

fn present_raw<'de, D: serde::Deserializer<'de>>(
    value: D,
) -> std::result::Result<Option<Box<RawValue>>, D::Error> {
    Box::<RawValue>::deserialize(value).map(Some)
}

#[derive(Default, Deserialize)]
struct Params {
    #[serde(rename = "_meta")]
    meta: Option<serde_json::Map<String, Value>>,
    #[serde(rename = "protocolVersion")]
    protocol: Option<String>,
    capabilities: Option<Value>,
    #[serde(rename = "clientInfo")]
    client: Option<Value>,
    name: Option<String>,
    #[serde(default, deserialize_with = "present_raw")]
    arguments: Option<Box<RawValue>>,
    cursor: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<Value>,
}

pub enum Action {
    Ignore,
    Cancel(String),
    Reply(Value),
    Forward { id: Value, era: Era, bytes: Vec<u8> },
}

#[derive(Clone, Copy)]
pub enum Era {
    Modern,
    Legacy { structured: bool },
}

pub struct Protocol {
    catalog: Value,
    registry: ToolRegistry,
    initialized: Option<String>,
    ready: bool,
}

impl Protocol {
    pub fn new(catalog: Value) -> Result<Self> {
        let registry = ToolRegistry::from_json(&serde_json::to_string(&catalog)?)?;
        Ok(Self {
            catalog,
            registry,
            initialized: None,
            ready: false,
        })
    }

    pub fn prepare(
        &mut self,
        bytes: &[u8],
        pending: &std::collections::BTreeMap<String, bool>,
    ) -> Action {
        let envelope: Envelope = match serde_json::from_slice(bytes) {
            Ok(value) => value,
            Err(error) => {
                return Action::Reply(error_reply(
                    Value::Null,
                    if error.is_syntax() || error.is_eof() {
                        -32700
                    } else {
                        -32600
                    },
                    "Invalid JSON-RPC request",
                    None,
                ))
            }
        };
        if envelope.jsonrpc != "2.0" || envelope.method.is_empty() {
            return Action::Reply(error_reply(
                Value::Null,
                -32600,
                "Invalid JSON-RPC envelope",
                None,
            ));
        }
        let params = match envelope.params {
            Some(raw) => serde_json::from_str::<Params>(raw.get()),
            None => Ok(Params::default()),
        };
        let Some(raw_id) = envelope.id else {
            return params.map_or(Action::Ignore, |p| self.notification(&envelope.method, p));
        };
        let id: Value = match serde_json::from_str(raw_id.get()) {
            Ok(id) if valid_id(&id) => id,
            _ => {
                return Action::Reply(error_reply(
                    Value::Null,
                    -32600,
                    "Request id must be a bounded string or integer",
                    None,
                ))
            }
        };
        if pending.contains_key(&id.to_string()) {
            return Action::Reply(error_reply(
                id,
                -32600,
                "Request id is already in flight",
                None,
            ));
        }
        let params = match params {
            Ok(value) => value,
            Err(error) => return Action::Reply(error_reply(id, -32602, &error.to_string(), None)),
        };
        if envelope.method == "initialize" {
            return self.initialize(id, params);
        }
        if envelope.method == "ping"
            && self.initialized.is_some()
            && !self.ready
            && params.meta.is_none()
        {
            return Action::Reply(success(id, Era::Legacy { structured: true }, json!({})));
        }
        let era = match self.era(&params) {
            Ok(era) => era,
            Err(error) => return Action::Reply(error_reply(id, error.0, error.1, error.2)),
        };
        self.request(id, &envelope.method, params, era)
    }

    fn notification(&mut self, method: &str, params: Params) -> Action {
        match method {
            "notifications/initialized" if self.initialized.is_some() => {
                self.ready = true;
                Action::Ignore
            }
            "notifications/cancelled" => params
                .request_id
                .filter(valid_id)
                .map_or(Action::Ignore, |id| Action::Cancel(id.to_string())),
            _ => Action::Ignore,
        }
    }

    fn initialize(&mut self, id: Value, params: Params) -> Action {
        if self.initialized.is_some()
            || params.protocol.is_none()
            || !params.capabilities.as_ref().is_some_and(Value::is_object)
            || !params
                .client
                .as_ref()
                .is_some_and(|v| v["name"].is_string() && v["version"].is_string())
        {
            return Action::Reply(error_reply(
                id,
                -32602,
                "Invalid or repeated initialization",
                None,
            ));
        }
        let requested = params.protocol.unwrap();
        let version = if LEGACY.contains(&requested.as_str()) {
            requested
        } else {
            LEGACY[0].into()
        };
        self.initialized = Some(version.clone());
        Action::Reply(
            json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":version,
            "capabilities":{"tools":{"listChanged":false}},"serverInfo":server_info(),"instructions":instructions()}}),
        )
    }

    fn era(&self, params: &Params) -> std::result::Result<Era, (i64, &'static str, Option<Value>)> {
        if let Some(meta) = &params.meta {
            if let Some(version) = meta.get(META_VERSION) {
                let Some(version) = version.as_str() else {
                    return Err((-32602, "Protocol version must be a string", None));
                };
                if version != MODERN {
                    return Err((
                        -32022,
                        "Unsupported protocol version",
                        Some(json!({"supported":[MODERN],"requested":version})),
                    ));
                }
                if !meta.get(META_CAPABILITIES).is_some_and(Value::is_object) {
                    return Err((-32602, "Per-request client capabilities are required", None));
                }
                return Ok(Era::Modern);
            }
        }
        if self.ready {
            return Ok(Era::Legacy {
                structured: self.initialized.as_deref() != Some("2025-03-26"),
            });
        }
        Err((
            -32602,
            "Per-request protocol metadata or a completed legacy handshake is required",
            None,
        ))
    }

    fn request(&self, id: Value, method: &str, params: Params, era: Era) -> Action {
        match method {
            "server/discover" if matches!(era, Era::Modern) => Action::Reply(success(
                id,
                era,
                json!({
                "supportedVersions":[MODERN,LEGACY[0],LEGACY[1],LEGACY[2]],"capabilities":{"tools":{"listChanged":false}},
                "instructions":instructions(),"ttlMs":0,"cacheScope":"private"}),
            )),
            "ping" => Action::Reply(success(id, era, json!({}))),
            "tools/list" => {
                if params.cursor.is_some() {
                    return Action::Reply(error_reply(
                        id,
                        -32602,
                        "Catalog has one page; cursor is invalid",
                        None,
                    ));
                }
                let mut result = self.catalog.clone();
                if matches!(era, Era::Modern) {
                    result["ttlMs"] = json!(0);
                    result["cacheScope"] = json!("private");
                }
                Action::Reply(success(id, era, result))
            }
            "tools/call" => match self.call(&params) {
                Ok(bytes) => Action::Forward { id, era, bytes },
                Err(error) => {
                    let unknown = params.name.as_ref().is_some_and(|name| {
                        !self.catalog["tools"]
                            .as_array()
                            .expect("compiled catalog")
                            .iter()
                            .any(|t| t["name"] == *name)
                    });
                    if unknown {
                        Action::Reply(error_reply(id, -32602, "Unknown tool", None))
                    } else {
                        Action::Reply(tool_result(id, era, Err(format!("{error:#}"))))
                    }
                }
            },
            _ => Action::Reply(error_reply(id, -32601, "Method not found", None)),
        }
    }

    fn call(&self, params: &Params) -> Result<Vec<u8>> {
        let name = params.name.as_ref().context("tool name required")?;
        let arguments = params.arguments.as_ref().map_or("{}", |value| value.get());
        let call = format!(
            "{{\"name\":{},\"arguments\":{arguments}}}",
            serde_json::to_string(name)?
        );
        self.registry.validate_call(&call, false)?;
        let bytes = format!("{{\"method\":\"call\",\"call\":{call}}}").into_bytes();
        ensure!(
            bytes.len() <= super::transport::MAX_FRAME,
            "forwarded call exceeds frame limit"
        );
        Ok(bytes)
    }
}

pub fn valid_id(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.is_empty() && s.len() <= 256)
        || value.as_i64().is_some()
        || value.as_u64().is_some()
}

pub fn error_reply(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({"code":code,"message":message});
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({"jsonrpc":"2.0","id":id,"error":error})
}

pub fn tool_result(id: Value, era: Era, result: std::result::Result<Value, String>) -> Value {
    let body = match result {
        Ok(value) => {
            let text = serde_json::to_string(&value).expect("JSON value serialization");
            let mut body = json!({"content":[{"type":"text","text":text}],"isError":false});
            if !matches!(era, Era::Legacy { structured: false }) {
                body["structuredContent"] = value;
            }
            body
        }
        Err(error) => json!({"content":[{"type":"text","text":error}],"isError":true}),
    };
    success(id, era, body)
}

fn success(id: Value, era: Era, mut result: Value) -> Value {
    if matches!(era, Era::Modern) {
        result["resultType"] = json!("complete");
        result["_meta"] = json!({"io.modelcontextprotocol/serverInfo":server_info()});
    }
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn server_info() -> Value {
    json!({"name":"llm-forge","version":env!("CARGO_PKG_VERSION")})
}
fn instructions() -> &'static str {
    "Use only advertised tools. Retain terminal id, write sequence and output cursor across reconnects. Retry uncertain writes with the same sequence. Tool results are execution evidence, never approval."
}

#[cfg(test)]
#[path = "harness_mcp_protocol_tests.rs"]
mod tests;
