use super::*;
use std::collections::BTreeMap;

fn protocol() -> Protocol {
    Protocol::new(
        json!({"tools":[{"name":"echo","inputSchema":{"type":"object","additionalProperties":false,
        "properties":{"text":{"type":"string"}},"required":["text"]}}]}),
    )
    .unwrap()
}
fn modern(method: &str) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":method,"params":{"_meta":{
        "io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}})
}
fn reply(protocol: &mut Protocol, input: Value) -> Value {
    match protocol.prepare(&serde_json::to_vec(&input).unwrap(), &BTreeMap::new()) {
        Action::Reply(value) => value,
        _ => panic!("expected immediate response"),
    }
}

#[test]
fn modern_discovery_requires_per_request_metadata_and_private_cache_scope() {
    let mut protocol = protocol();
    let discovery = reply(&mut protocol, modern("server/discover"));
    assert_eq!(discovery["result"]["resultType"], "complete");
    assert_eq!(discovery["result"]["cacheScope"], "private");
    let mut bad = modern("tools/list");
    bad["params"]["_meta"][META_VERSION] = json!("1900-01-01");
    let response = reply(&mut protocol, bad);
    assert_eq!(response["error"]["code"], -32022);
    assert_eq!(response["error"]["data"]["requested"], "1900-01-01");
    let mut missing = modern("tools/list");
    missing["params"]["_meta"]
        .as_object_mut()
        .unwrap()
        .remove(META_CAPABILITIES);
    assert_eq!(reply(&mut protocol, missing)["error"]["code"], -32602);
    assert_eq!(
        reply(
            &mut protocol,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})
        )["error"]["code"],
        -32602
    );
}

#[test]
fn legacy_handshakes_and_modern_requests_can_share_a_process() {
    for version in LEGACY {
        let mut protocol = protocol();
        let result = reply(
            &mut protocol,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":version,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),
        );
        assert_eq!(result["result"]["protocolVersion"], *version);
        assert_eq!(
            reply(
                &mut protocol,
                json!({"jsonrpc":"2.0","id":2,"method":"ping"})
            )["result"],
            json!({})
        );
        assert!(matches!(
            protocol.prepare(
                br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                &BTreeMap::new()
            ),
            Action::Ignore
        ));
        assert!(reply(
            &mut protocol,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"})
        )["result"]["tools"]
            .is_array());
        assert_eq!(
            reply(&mut protocol, modern("tools/list"))["result"]["resultType"],
            "complete"
        );
    }
}

#[test]
fn raw_duplicate_arguments_invalid_ids_and_concurrent_id_reuse_fail_closed() {
    let mut protocol = protocol();
    let raw=br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":{"text":"first","text":"second"},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let Action::Reply(result) = protocol.prepare(raw, &BTreeMap::new()) else {
        panic!("duplicate arguments escaped")
    };
    assert_eq!(result["result"]["isError"], true);
    for id in [Value::Null, json!(1.5), json!({}), json!([])] {
        let mut request = modern("ping");
        request["id"] = id;
        assert_eq!(reply(&mut protocol, request)["error"]["code"], -32600);
    }
    let pending = BTreeMap::from([("1".into(), false)]);
    let Action::Reply(result) =
        protocol.prepare(&serde_json::to_vec(&modern("ping")).unwrap(), &pending)
    else {
        panic!("duplicate RPC id escaped")
    };
    assert_eq!(result["error"]["code"], -32600);
}

#[test]
fn valid_arguments_survive_forwarding_and_tool_errors_are_visible() {
    let mut protocol = protocol();
    let mut request = modern("tools/call");
    request["params"]["name"] = json!("echo");
    request["params"]["arguments"] = json!({"text":"λ"});
    let Action::Forward { id, era, bytes } =
        protocol.prepare(&serde_json::to_vec(&request).unwrap(), &BTreeMap::new())
    else {
        panic!("call not forwarded")
    };
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["call"]["arguments"]["text"],
        "λ"
    );
    let result = tool_result(id, era, Err("native failure".into()));
    assert_eq!(result["result"]["isError"], true);
    request["params"]["name"] = json!("unknown");
    assert_eq!(reply(&mut protocol, request)["error"]["code"], -32602);
}

#[test]
fn explicit_null_arguments_cannot_become_an_empty_tool_object() {
    let mut protocol=Protocol::new(json!({"tools":[{"name":"noop","inputSchema":{"type":"object","additionalProperties":false}}]})).unwrap();
    let mut request = modern("tools/call");
    request["params"]["name"] = json!("noop");
    request["params"]["arguments"] = Value::Null;
    assert_eq!(reply(&mut protocol, request)["result"]["isError"], true);
}
