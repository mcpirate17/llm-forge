//! Bounded, single-flight MCP 2025-11-25 client protocol. No process or I/O ownership.
//! Schema pinning compares JSON values structurally, not semantic equivalence.
use crate::tool_schema::{parse_document, CompiledSchema};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;

pub const MAX_FRAME: usize = 1024 * 1024;
const MAX_PAGES: usize = 32;
const MAX_TOOLS: usize = 1024;
const MAX_CURSOR: usize = 4096;
const VERSION: &str = "2025-11-25";

#[derive(Debug, PartialEq)]
pub enum Reply {
    Result(Value),
    RpcError {
        code: i64,
        message: String,
        data: Option<Value>,
    },
}
#[derive(Clone, Copy, PartialEq)]
enum Stage {
    New,
    Init,
    Initialized,
    List,
    Listed,
    Call,
    Failed,
}

pub struct OutboundProtocol {
    name: String,
    input: Value,
    input_validator: CompiledSchema,
    output: Option<Value>,
    output_validator: Option<CompiledSchema>,
    stage: Stage,
    next_id: u64,
    pending: u64,
    pages: usize,
    tools: usize,
    cursors: HashSet<String>,
    cursor: Option<String>,
    found: bool,
}
impl OutboundProtocol {
    pub fn new(name: &str, input_json: &str, output_json: Option<&str>) -> Result<Self> {
        ensure!(
            !name.is_empty()
                && name.len() <= 128
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
            "invalid pinned tool name"
        );
        let input = parse_document(input_json)?;
        ensure!(
            input.get("type") == Some(&json!("object")),
            "input schema must describe an object"
        );
        let input_validator = CompiledSchema::compile(&input)?;
        let output = output_json.map(parse_document).transpose()?;
        if let Some(value) = &output {
            ensure!(
                value.get("type") == Some(&json!("object")),
                "output schema must describe an object"
            );
        }
        let output_validator = output.as_ref().map(CompiledSchema::compile).transpose()?;
        Ok(Self {
            name: name.into(),
            input,
            input_validator,
            output,
            output_validator,
            stage: Stage::New,
            next_id: 1,
            pending: 0,
            pages: 0,
            tools: 0,
            cursors: HashSet::new(),
            cursor: None,
            found: false,
        })
    }
    fn request(&mut self, method: &str, params: Value, stage: Stage) -> Result<Vec<u8>> {
        let id = self.next_id;
        let next = id.checked_add(1).context("MCP request id exhausted")?;
        let bytes = frame(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        self.next_id = next;
        self.pending = id;
        self.stage = stage;
        Ok(bytes)
    }
    pub fn initialize_request(&mut self) -> Result<Vec<u8>> {
        ensure!(self.stage == Stage::New, "unexpected initialize");
        self.request("initialize", json!({"protocolVersion":VERSION,"capabilities":{},"clientInfo":{"name":"forge","version":env!("CARGO_PKG_VERSION")}}), Stage::Init)
    }
    pub fn accept_initialize(&mut self, bytes: &[u8]) -> Result<Vec<u8>> {
        let result = self.response(bytes, Stage::Init)?;
        let value = success(result)?;
        let object = value
            .as_object()
            .context("initialize result must be object")?;
        ensure!(
            object.get("protocolVersion") == Some(&json!(VERSION)),
            "unsupported MCP protocol version"
        );
        let caps = object
            .get("capabilities")
            .and_then(Value::as_object)
            .context("missing capabilities")?;
        ensure!(
            caps.get("tools").is_some_and(Value::is_object),
            "server lacks tools capability"
        );
        let info = object
            .get("serverInfo")
            .and_then(Value::as_object)
            .context("missing serverInfo")?;
        ensure!(
            info.get("name")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
                && info
                    .get("version")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty()),
            "invalid serverInfo"
        );
        self.stage = Stage::Initialized;
        frame(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
    }
    pub fn list_request(&mut self) -> Result<Vec<u8>> {
        ensure!(self.stage == Stage::Initialized, "unexpected tools/list");
        ensure!(self.pages < MAX_PAGES, "MCP page limit exceeded");
        let params = self
            .cursor
            .as_ref()
            .map_or(json!({}), |c| json!({"cursor":c}));
        self.request("tools/list", params, Stage::List)
    }
    /// Returns true after the final page and only if the selected tool is pinned.
    pub fn accept_list(&mut self, bytes: &[u8]) -> Result<bool> {
        let response = self.response(bytes, Stage::List)?;
        let value = success(response)?;
        let object = value
            .as_object()
            .context("tools/list result must be object")?;
        let tools = object
            .get("tools")
            .and_then(Value::as_array)
            .context("missing tools array")?;
        self.pages += 1;
        self.tools = self
            .tools
            .checked_add(tools.len())
            .context("tool count overflow")?;
        ensure!(self.tools <= MAX_TOOLS, "MCP tool limit exceeded");
        for tool in tools {
            let t = tool.as_object().context("tool must be object")?;
            let name = t
                .get("name")
                .and_then(Value::as_str)
                .context("tool name missing")?;
            ensure!(
                t.get("inputSchema").is_some_and(Value::is_object),
                "tool inputSchema missing"
            );
            if name == self.name {
                ensure!(!self.found, "duplicate selected tool");
                ensure!(
                    t.get("inputSchema") == Some(&self.input),
                    "pinned input schema drift"
                );
                ensure!(
                    t.get("outputSchema") == self.output.as_ref(),
                    "pinned output schema drift"
                );
                if let Some(execution) = t.get("execution") {
                    let execution = execution.as_object().context("invalid execution")?;
                    ensure!(
                        execution
                            .get("taskSupport")
                            .is_none_or(|v| v == "forbidden" || v == "optional"),
                        "required or invalid task support"
                    );
                }
                self.found = true;
            }
        }
        self.cursor = match object.get("nextCursor") {
            None => None,
            Some(v) => {
                let c = v.as_str().context("invalid cursor")?;
                ensure!(
                    !c.is_empty() && c.len() <= MAX_CURSOR,
                    "invalid cursor length"
                );
                ensure!(self.cursors.insert(c.to_owned()), "MCP cursor loop");
                Some(c.to_owned())
            }
        };
        if self.cursor.is_some() {
            self.stage = Stage::Initialized;
            return Ok(false);
        }
        ensure!(self.found, "pinned tool not found");
        self.stage = Stage::Listed;
        Ok(true)
    }
    pub fn call_request(&mut self, arguments_json: &str) -> Result<Vec<u8>> {
        ensure!(self.stage == Stage::Listed, "unexpected tools/call");
        let arguments = parse_document(arguments_json)?;
        ensure!(arguments.is_object(), "arguments must be object");
        self.input_validator.validate(arguments_json)?;
        self.request(
            "tools/call",
            json!({"name":self.name,"arguments":arguments}),
            Stage::Call,
        )
    }
    pub fn accept_call(&mut self, bytes: &[u8]) -> Result<Reply> {
        let reply = self.response(bytes, Stage::Call)?;
        if let Reply::Result(value) = &reply {
            self.validate_result(value)?;
            self.stage = Stage::Listed;
        }
        // RPC errors latch Failed; the caller closes the endpoint without retrying.
        Ok(reply)
    }
    fn validate_result(&self, value: &Value) -> Result<()> {
        let o = value.as_object().context("call result must be object")?;
        let error = match o.get("isError") {
            None => false,
            Some(v) => v.as_bool().context("isError must be boolean")?,
        };
        let content = o
            .get("content")
            .and_then(Value::as_array)
            .context("content must be array")?;
        for item in content {
            let item = item.as_object().context("content item must be object")?;
            ensure!(
                item.get("type").and_then(Value::as_str) == Some("text"),
                "unsupported MCP content type"
            );
            ensure!(
                item.get("text").is_some_and(Value::is_string),
                "text content missing text"
            );
        }
        if let Some(structured) = o.get("structuredContent") {
            ensure!(structured.is_object(), "structuredContent must be object");
        }
        if !error {
            if let Some(schema) = &self.output_validator {
                let structured = o
                    .get("structuredContent")
                    .context("missing structuredContent")?;
                schema.validate(&serde_json::to_string(structured)?)?;
            }
        }
        Ok(())
    }
    fn response(&mut self, bytes: &[u8], expected: Stage) -> Result<Reply> {
        ensure!(self.stage == expected, "unexpected MCP response state");
        self.stage = Stage::Failed;
        ensure!(bytes.len() <= MAX_FRAME, "MCP frame too large");
        let value = parse_document(std::str::from_utf8(bytes)?)?;
        let obj = value.as_object().context("MCP envelope must be object")?;
        ensure!(
            obj.get("jsonrpc") == Some(&json!("2.0")),
            "invalid JSON-RPC version"
        );
        ensure!(
            !obj.contains_key("method"),
            "unsolicited server request or notification"
        );
        ensure!(
            obj.get("id") == Some(&json!(self.pending)),
            "unexpected JSON-RPC id"
        );
        ensure!(
            obj.keys()
                .all(|k| ["jsonrpc", "id", "result", "error"].contains(&k.as_str())),
            "unknown envelope field"
        );
        match (obj.get("result"), obj.get("error")) {
            (Some(v), None) if v.is_object() => Ok(Reply::Result(v.clone())),
            (None, Some(e)) => {
                let e = e.as_object().context("error must be object")?;
                ensure!(
                    e.keys()
                        .all(|k| ["code", "message", "data"].contains(&k.as_str())),
                    "unknown error field"
                );
                let code = e
                    .get("code")
                    .and_then(Value::as_i64)
                    .context("invalid error code")?;
                let message = e
                    .get("message")
                    .and_then(Value::as_str)
                    .context("invalid error message")?
                    .to_owned();
                Ok(Reply::RpcError {
                    code,
                    message,
                    data: e.get("data").cloned(),
                })
            }
            _ => bail!("response must contain exactly one result or error"),
        }
    }
}
fn success(reply: Reply) -> Result<Value> {
    match reply {
        Reply::Result(value) => Ok(value),
        Reply::RpcError { code, message, .. } => bail!("MCP RPC error {code}: {message}"),
    }
}
fn frame(value: Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(&value)?;
    ensure!(bytes.len() <= MAX_FRAME, "MCP request too large");
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    const SCHEMA: &str = r#"{"type":"object","properties":{"x":{"type":"string"}},"required":["x"],"additionalProperties":false}"#;
    fn protocol() -> OutboundProtocol {
        OutboundProtocol::new("selected", SCHEMA, None).unwrap()
    }
    fn init(p: &mut OutboundProtocol) {
        p.initialize_request().unwrap();
        p.accept_initialize(br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"s","version":"1"}}}"#).unwrap();
    }
    fn selected() -> Value {
        json!({"name":"selected","inputSchema":parse_document(SCHEMA).unwrap()})
    }
    fn page(
        p: &mut OutboundProtocol,
        id: u64,
        tools: Vec<Value>,
        cursor: Option<&str>,
    ) -> Result<bool> {
        p.list_request()?;
        let mut result = json!({"tools":tools});
        if let Some(c) = cursor {
            result["nextCursor"] = json!(c);
        }
        p.accept_list(
            serde_json::to_string(&json!({"jsonrpc":"2.0","id":id,"result":result}))?.as_bytes(),
        )
    }
    #[test]
    fn full_paginated_call_and_remote_errors() {
        let mut p = protocol();
        init(&mut p);
        assert!(!page(&mut p, 2, vec![], Some("opaque")).unwrap());
        assert!(page(&mut p, 3, vec![selected()], None).unwrap());
        p.call_request(r#"{"x":"ok"}"#).unwrap();
        assert!(matches!(p.accept_call(br#"{"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"failed"}],"isError":true}}"#).unwrap(), Reply::Result(_)));
        let mut p = protocol();
        init(&mut p);
        page(&mut p, 2, vec![selected()], None).unwrap();
        p.call_request(r#"{"x":"ok"}"#).unwrap();
        assert!(matches!(
            p.accept_call(br#"{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"bad"}}"#)
                .unwrap(),
            Reply::RpcError { code: -32602, .. }
        ));
    }
    #[test]
    fn sequential_calls_and_failure_latch() {
        let mut p = protocol();
        init(&mut p);
        page(&mut p, 2, vec![selected()], None).unwrap();
        p.call_request(r#"{"x":"first"}"#).unwrap();
        p.accept_call(br#"{"jsonrpc":"2.0","id":3,"result":{"content":[],"isError":true}}"#)
            .unwrap();
        let request = p.call_request(r#"{"x":"second"}"#).unwrap();
        assert_eq!(
            parse_document(std::str::from_utf8(&request).unwrap()).unwrap()["id"],
            4
        );
        assert!(p
            .accept_call(br#"{"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"image"}]}}"#)
            .is_err());
        assert!(p.call_request(r#"{"x":"third"}"#).is_err());
        let mut p = protocol();
        init(&mut p);
        page(&mut p, 2, vec![selected()], None).unwrap();
        p.call_request(r#"{"x":"first"}"#).unwrap();
        assert!(matches!(
            p.accept_call(br#"{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"bad"}}"#)
                .unwrap(),
            Reply::RpcError { .. }
        ));
        assert!(p.call_request(r#"{"x":"second"}"#).is_err());
    }
    #[test]
    fn invalid_envelopes_and_unsolicited_requests_fail_closed() {
        for response in [
            r#"{"jsonrpc":"1.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":1,"id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"sampling/createMessage"}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{}}"#,
            "{",
        ] {
            let mut p = protocol();
            p.initialize_request().unwrap();
            assert!(
                p.accept_initialize(response.as_bytes()).is_err(),
                "{response}"
            );
            assert!(p.accept_initialize(response.as_bytes()).is_err());
        }
        let mut p = protocol();
        p.initialize_request().unwrap();
        assert!(p.accept_initialize(br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"other","capabilities":{"tools":{}},"serverInfo":{"name":"s","version":"1"}}}"#).is_err());
    }
    #[test]
    fn discovery_bounds_and_schema_drift() {
        let mut p = protocol();
        init(&mut p);
        assert!(!page(&mut p, 2, vec![], Some("repeat")).unwrap());
        assert!(page(&mut p, 3, vec![], Some("repeat")).is_err());
        let mut p = protocol();
        init(&mut p);
        assert!(page(&mut p, 2, vec![selected(), selected()], None).is_err());
        let mut p = protocol();
        init(&mut p);
        assert!(page(&mut p, 2, vec![], None).is_err());
        let mut p = protocol();
        init(&mut p);
        assert!(page(
            &mut p,
            2,
            vec![json!({"name":"selected","inputSchema":{"type":"object"}})],
            None
        )
        .is_err());
        let mut p = protocol();
        init(&mut p);
        for id in 2..(2 + MAX_PAGES as u64) {
            assert!(!page(&mut p, id, vec![], Some(&format!("{id}"))).unwrap());
        }
        assert!(p.list_request().is_err());
        let mut p = protocol();
        init(&mut p);
        assert!(page(&mut p, 2, vec![selected(); MAX_TOOLS + 1], None).is_err());
    }
    #[test]
    fn arguments_and_output_schema_enforced() {
        let mut p = OutboundProtocol::new("selected", SCHEMA, Some(SCHEMA)).unwrap();
        init(&mut p);
        let mut t = selected();
        t["outputSchema"] = parse_document(SCHEMA).unwrap();
        page(&mut p, 2, vec![t], None).unwrap();
        assert!(p.call_request(r#"{"x":"a","x":"b"}"#).is_err());
        assert!(p.call_request(r#"{"x":42}"#).is_err());
        p.call_request(r#"{"x":"ok"}"#).unwrap();
        assert!(p
            .accept_call(
                br#"{"jsonrpc":"2.0","id":3,"result":{"content":[],"structuredContent":{"x":42}}}"#
            )
            .is_err());
    }
}
