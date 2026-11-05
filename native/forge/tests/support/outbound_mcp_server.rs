//! External, intentionally adversarial MCP fixture; never part of the Forge CLI.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{self, BufRead, Read, Write};
use std::time::Duration;
const MAX_FRAME: usize = 1_048_576;

fn input_schema() -> Value {
    json!({"type":"object","properties":{"path":{"type":"string","minLength":1,
        "maxLength":4096}},"required":["path"],"additionalProperties":false})
}
fn output_schema() -> Value {
    json!({"type":"object","properties":{
        "pid":{"type":"integer","minimum":1,"maximum":4294967295_u64},
        "call_count":{"type":"integer","minimum":1,"maximum":32},
        "bytes":{"type":"integer","minimum":0,"maximum":4096},
        "sha256":{"type":"string","minLength":64,"maxLength":64},
        "text":{"type":"string","maxLength":4096},"secret_present":{"type":"boolean"}},
        "required":["pid","call_count","bytes","sha256","text","secret_present"],
        "additionalProperties":false})
}
fn send(value: &Value) -> io::Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, value)?;
    out.write_all(b"\n")?;
    out.flush()
}
fn result(id: &Value, value: Value) -> io::Result<()> {
    send(&json!({"jsonrpc":"2.0","id":id,"result":value}))
}
fn read_file(path: &str, calls: usize) -> Result<Value, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 4096 {
        return Err("fixture file exceeds 4096 bytes".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    Ok(
        json!({"pid":std::process::id(),"call_count":calls,"bytes":bytes.len(),
        "sha256":format!("{:x}",Sha256::digest(&bytes)),"text":text,
        "secret_present":std::env::var_os("FORGE_OUTBOUND_SYNTHETIC_SECRET").is_some()}),
    )
}
fn wait_gate(id: &Value, response: &Value) -> io::Result<()> {
    let gate = std::env::args()
        .nth(2)
        .ok_or_else(|| io::Error::other("gated mode requires operator gate"))?;
    if !std::path::Path::new(&gate).is_absolute() {
        return Err(io::Error::other("gate must be absolute"));
    }
    let witness =
        json!({"request_id":id,"response_sha256":response["structuredContent"]["sha256"]});
    // The only write is inside the fixture's already owned writable cwd. The
    // operator gate is separately read-granted and never a pinned input.
    std::fs::write("request-started.json", serde_json::to_vec(&witness)?)?;
    let started = std::time::Instant::now();
    loop {
        let mut bytes = Vec::new();
        std::fs::File::open(&gate)?
            .take(32)
            .read_to_end(&mut bytes)?;
        match bytes.as_slice() {
            b"release\n" => return Ok(()),
            b"blocked\n" => {}
            _ => return Err(io::Error::other("unexpected operator gate bytes")),
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(io::Error::other("operator gate timeout"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn call(mode: &str, id: &Value, args: &Value, calls: usize) -> io::Result<()> {
    match mode {
        "hang" => std::thread::sleep(Duration::from_secs(10)),
        "early_exit" => return Err(io::Error::other("intentional early exit")),
        "partial" => {
            io::stdout().write_all(b"{\"jsonrpc\":")?;
            return Err(io::Error::other("partial frame"));
        }
        "oversized" => {
            io::stdout().write_all(&vec![b'x'; MAX_FRAME + 1])?;
            io::stdout().flush()?;
            return Ok(());
        }
        "stderr_flood" => {
            io::stderr().write_all(&vec![b'x'; MAX_FRAME + 1])?;
            io::stderr().flush()?;
        }
        "unsolicited" => send(
            &json!({"jsonrpc":"2.0","id":"server-request","method":"sampling/createMessage","params":{}}),
        )?,
        _ => {}
    }
    let response = match read_file(args["path"].as_str().unwrap_or(""), calls) {
        Ok(mut data) => {
            if mode == "bad_output" {
                data["bytes"] = json!("not an integer");
            }
            json!({"content":[{"type":"text","text":serde_json::to_string(&data)?}],
                "structuredContent":data,"isError":false})
        }
        Err(error) => json!({"content":[{"type":"text","text":error}],"isError":true}),
    };
    if mode == "gated" {
        wait_gate(id, &response)?;
    }
    let reply_id = if mode == "wrong_id" {
        json!(999999)
    } else {
        id.clone()
    };
    result(&reply_id, response)?;
    if mode == "idle_unsolicited" {
        send(
            &json!({"jsonrpc":"2.0","id":"idle-request","method":"sampling/createMessage","params":{}}),
        )?;
    }
    if mode == "idle_eof" {
        return Err(io::Error::other("intentional idle EOF"));
    }
    Ok(())
}
fn serve(mode: &str) -> io::Result<()> {
    let input = io::stdin();
    let mut input = input.lock();
    let mut initialized = false;
    let mut calls = 0;
    for _ in 0..40 {
        let mut bytes = Vec::new();
        let n = input
            .by_ref()
            .take((MAX_FRAME + 1) as u64)
            .read_until(b'\n', &mut bytes)?;
        if n == 0 {
            return Ok(());
        }
        if n > MAX_FRAME || bytes.last() != Some(&b'\n') {
            return Err(io::Error::other("invalid input frame"));
        }
        let request: Value = serde_json::from_slice(&bytes)?;
        let id = &request["id"];
        match request["method"].as_str() {
            Some("initialize") => {
                if request["params"]["protocolVersion"] != "2025-11-25" {
                    return Err(io::Error::other("unexpected revision"));
                }
                let version = if mode == "bad_version" {
                    "1900-01-01"
                } else {
                    "2025-11-25"
                };
                result(
                    id,
                    json!({"protocolVersion":version,"capabilities":{"tools":{}},
                    "serverInfo":{"name":"forge-outbound-fixture","version":"1"}}),
                )?;
            }
            Some("notifications/initialized") if id.is_null() => initialized = true,
            Some("tools/list") if initialized => {
                let mut input = input_schema();
                let mut output = output_schema();
                if mode == "wrong_input_schema" {
                    input["properties"]["path"]["maxLength"] = json!(4095);
                }
                if mode == "wrong_output_schema" {
                    output["properties"]["bytes"]["type"] = json!("string");
                }
                result(
                    id,
                    json!({"tools":[{"name":"read_file","description":"Read fixture bytes",
                    "inputSchema":input,"outputSchema":output},
                    {"name":"unselected_write","inputSchema":{"type":"object"}}]}),
                )?;
            }
            Some("tools/call") if initialized && request["params"]["name"] == "read_file" => {
                calls += 1;
                if calls > 32 {
                    return Err(io::Error::other("call budget exhausted"));
                }
                call(mode, id, &request["params"]["arguments"], calls)?;
            }
            _ => return Err(io::Error::other("unexpected protocol request")),
        }
    }
    Err(io::Error::other("request budget exhausted"))
}
fn main() -> io::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "normal".into());
    if ![
        "normal",
        "gated",
        "hang",
        "early_exit",
        "partial",
        "oversized",
        "stderr_flood",
        "unsolicited",
        "wrong_id",
        "idle_unsolicited",
        "idle_eof",
        "bad_version",
        "bad_output",
        "wrong_input_schema",
        "wrong_output_schema",
    ]
    .contains(&mode.as_str())
    {
        return Err(io::Error::other("unknown fixture mode"));
    }
    serve(&mode)
}
