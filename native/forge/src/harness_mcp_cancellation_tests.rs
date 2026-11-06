//! Adapter wire controls only: fake Unix harness, no worker/cgroup/client proof.
use super::*;
use serde_json::json;
use std::io::ErrorKind;
use tokio::io::{AsyncReadExt, DuplexStream};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(3);

async fn send(output: &mut (impl AsyncWrite + Unpin), value: Value) {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    tokio::time::timeout(WAIT, output.write_all(&bytes))
        .await
        .unwrap()
        .unwrap();
}

async fn frame(input: &mut (impl AsyncBufRead + Unpin)) -> Value {
    let mut bytes = Vec::new();
    assert!(
        tokio::time::timeout(WAIT, transport::read_frame(input, &mut bytes, 8192))
            .await
            .unwrap()
            .unwrap()
    );
    serde_json::from_slice(&bytes).unwrap()
}

fn catalog() -> Value {
    json!({"tools":[
        {"name":"python_test_report","inputSchema":{"type":"object","additionalProperties":false,
            "properties":{"max_output_bytes":{"type":"integer","minimum":512,"maximum":262144}},
            "required":["max_output_bytes"]}},
        {"name":"echo","inputSchema":{"type":"object","additionalProperties":false,
            "properties":{"text":{"type":"string"}},"required":["text"]}}
    ]})
}

struct Wire {
    _root: tempfile::TempDir,
    listener: Option<UnixListener>,
    input: Option<DuplexStream>,
    output: BufReader<DuplexStream>,
    adapter: Option<JoinHandle<Result<()>>>,
}
impl Wire {
    async fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("harness.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (input, reader) = tokio::io::duplex(16384);
        let (writer, output) = tokio::io::duplex(16384);
        let adapter = tokio::spawn(super::serve(socket, BufReader::new(reader), writer));
        let (connection, _) = tokio::time::timeout(WAIT, listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut connection = BufReader::new(connection);
        assert_eq!(frame(&mut connection).await, json!({"method":"catalog"}));
        send(connection.get_mut(), json!({"ok":true,"result":catalog()})).await;
        drop(connection);
        let mut wire = Self {
            _root: root,
            listener: Some(listener),
            input: Some(input),
            output: BufReader::new(output),
            adapter: Some(adapter),
        };
        wire.write(
            json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":{},
            "clientInfo":{"name":"adapter-wire-test","version":"1"}}}),
        )
        .await;
        assert_eq!(wire.read().await["result"]["protocolVersion"], "2025-11-25");
        wire.write(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        wire
    }
    async fn write(&mut self, value: Value) {
        send(self.input.as_mut().unwrap(), value).await;
    }
    async fn read(&mut self) -> Value {
        frame(&mut self.output).await
    }
    async fn ping(&mut self, id: u64) {
        self.write(json!({"jsonrpc":"2.0","id":id,"method":"ping"}))
            .await;
        assert_eq!(
            self.read().await,
            json!({"jsonrpc":"2.0","id":id,"result":{}})
        );
    }
    async fn call(&mut self, name: &str, arguments: Value) -> BufReader<UnixStream> {
        self.write(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}}))
            .await;
        let (connection, _) = tokio::time::timeout(WAIT, self.listener.as_ref().unwrap().accept())
            .await
            .unwrap()
            .unwrap();
        let mut connection = BufReader::new(connection);
        assert_eq!(
            frame(&mut connection).await,
            json!({"method":"call","call":{"name":name,"arguments":arguments}})
        );
        connection
    }
    async fn cancel(&mut self) {
        self.write(json!({"jsonrpc":"2.0","method":"notifications/cancelled",
            "params":{"requestId":1,"reason":"wire test"}}))
            .await;
    }
    async fn eof(&mut self) {
        self.input.take().unwrap().shutdown().await.unwrap();
    }
    async fn finish(mut self) {
        if self.input.is_some() {
            self.eof().await;
        }
        tokio::time::timeout(WAIT, self.adapter.take().unwrap())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let mut remainder = Vec::new();
        tokio::time::timeout(
            WAIT,
            (&mut self.output).take(8193).read_to_end(&mut remainder),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            remainder.is_empty(),
            "unexpected late adapter reply: {}",
            String::from_utf8_lossy(&remainder)
        );
        // With serve finished, no future can create another harness connection.
        let listener = self.listener.take().unwrap().into_std().unwrap();
        assert!(matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock));
    }
}
impl Drop for Wire {
    fn drop(&mut self) {
        if let Some(adapter) = self.adapter.take() {
            adapter.abort();
        }
    }
}

async fn connection_eof(connection: &mut BufReader<UnixStream>) {
    let mut byte = [0u8; 1];
    assert_eq!(
        tokio::time::timeout(WAIT, connection.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

// Reusing the cancelled request ID is a protocol-state barrier: once ping is
// accepted, serve has joined/removed the old worker. No timing delay is assumed.
async fn cancelled_worker_joined(wire: &mut Wire) {
    tokio::time::timeout(WAIT, async {
        loop {
            wire.write(json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
                .await;
            let reply = wire.read().await;
            assert_eq!(reply["id"], 1);
            if reply.get("error").is_none() {
                assert_eq!(
                    reply["result"],
                    json!({}),
                    "cancelled tool result escaped suppression"
                );
                break;
            }
            assert_eq!(reply["error"]["code"], -32600);
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn validated_report_cancellation_closes_call_socket_and_keeps_ping_live() {
    let mut wire = Wire::start().await;
    let mut connection = wire
        .call("python_test_report", json!({"max_output_bytes":4096}))
        .await;
    wire.cancel().await;
    connection_eof(&mut connection).await;
    cancelled_worker_joined(&mut wire).await;
    wire.ping(2).await;
    wire.finish().await;
}

#[tokio::test]
async fn adapter_stdio_eof_closes_an_in_flight_report_socket() {
    let mut wire = Wire::start().await;
    let mut connection = wire
        .call("python_test_report", json!({"max_output_bytes":4096}))
        .await;
    wire.eof().await;
    connection_eof(&mut connection).await;
    wire.finish().await;
}

#[tokio::test]
async fn ordinary_tool_cancellation_retains_work_but_suppresses_its_eventual_reply() {
    let mut wire = Wire::start().await;
    let mut connection = wire
        .call("echo", json!({"text":"continue owned work"}))
        .await;
    wire.cancel().await;
    // Ping is a barrier after the notification; the fake service has not replied.
    wire.ping(2).await;
    let mut byte = [0u8; 1];
    assert!(matches!(connection.get_ref().try_read(&mut byte),
        Err(error) if error.kind() == ErrorKind::WouldBlock));
    // Release the fake service only after proving the ordinary connection lives.
    send(
        connection.get_mut(),
        json!({"ok":true,"result":{"text":"completed"}}),
    )
    .await;
    connection_eof(&mut connection).await;
    cancelled_worker_joined(&mut wire).await;
    wire.ping(3).await;
    wire.finish().await;
}

#[tokio::test]
async fn malformed_and_unknown_report_arguments_are_never_forwarded() {
    let mut wire = Wire::start().await;
    for (offset, arguments) in [
        json!({}),
        json!({"max_output_bytes":"4096"}),
        json!({"max_output_bytes":511}),
        json!({"max_output_bytes":4096,"unknown":true}),
    ]
    .into_iter()
    .enumerate()
    {
        let id = offset + 1;
        wire.write(json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":"python_test_report","arguments":arguments}}))
            .await;
        let reply = wire.read().await;
        assert_eq!(reply["id"], id);
        assert_eq!(reply["result"]["isError"], true);
    }
    wire.write(json!({"jsonrpc":"2.0","id":10,"method":"tools/call",
        "params":{"name":"unknown_report","arguments":{"max_output_bytes":4096}}}))
        .await;
    assert_eq!(wire.read().await["error"]["code"], -32602);
    wire.ping(11).await;
    wire.finish().await;
}
