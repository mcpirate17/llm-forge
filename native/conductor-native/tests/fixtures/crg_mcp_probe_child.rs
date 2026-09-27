//! Test-only stdio MCP peer for the production CRG probe contract.

use serde_json::{json, Value};
use std::env;
use std::io::{self, BufRead, Write};

fn answer(
    message: &Value,
    notified: bool,
    tools: usize,
    leak: &str,
    call_error: bool,
    require_notify: bool,
) -> Option<Value> {
    let id = message.get("id")?.clone();
    match message.get("method")?.as_str()? {
        "initialize" => Some(json!({
            "jsonrpc":"2.0", "id":id, "result":{
                "protocolVersion":"2024-11-05", "capabilities":{},
                "serverInfo":{"name":"fake","version":"0"}
            }
        })),
        "tools/list" if require_notify && !notified => Some(json!({
            "jsonrpc":"2.0", "id":id,
            "error":{"code":-32002,"message":"not initialized"}
        })),
        "tools/list" => Some(json!({
            "jsonrpc":"2.0", "id":id,
            "result":{"tools":(0..tools).map(|i| json!({"name":format!("tool_{i}")})).collect::<Vec<_>>()}
        })),
        "tools/call" if call_error => Some(json!({
            "jsonrpc":"2.0", "id":id,
            "error":{"code":-32000,"message":"boom"}
        })),
        "tools/call" => Some(json!({
            "jsonrpc":"2.0", "id":id,
            "result":{"content":[{"type":"text","text":if leak.is_empty(){"42 nodes"}else{leak}}]}
        })),
        _ => None,
    }
}

fn main() -> io::Result<()> {
    let tools = env::var("FAKE_TOOLS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let leak = env::var("FAKE_LEAK").unwrap_or_default();
    let call_error = env::var("FAKE_CALL_ERROR").is_ok_and(|v| !v.is_empty());
    let require_notify = env::var("FAKE_REQUIRE_NOTIFY").unwrap_or_else(|_| "1".to_owned()) == "1";
    let mut notified = false;
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message.get("method").and_then(Value::as_str) == Some("notifications/initialized") {
            notified = true;
            continue;
        }
        if let Some(response) = answer(&message, notified, tools, &leak, call_error, require_notify)
        {
            writeln!(io::stdout().lock(), "{response}")?;
        }
    }
    Ok(())
}
