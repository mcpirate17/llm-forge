#![cfg(target_os = "linux")]
#[path = "support/embedding_fixture.rs"]
mod fixture;
#[allow(dead_code)]
mod support;

use forge::embedding_broker::{config::RoutingConfig, environment};
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    net::{SocketAddr, TcpListener},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

fn port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn routes(host: &support::Host, endpoint: &str) -> String {
    let text = format!(
        r#"schema_version = 1
[policy]
primary_route = "local"
fallback_routes = []
[[route]]
route_id = "local"
backend_kind = "ollama-local"
protocol = "ollama-embed"
priority = 1
normalization = "l2-client"
tokenizer_policy = "backend-default"
truncation_policy = "unicode-prefix"
max_input_chars = 2000
device_policy = "gpu-guest-or-cpu"
paid = false
endpoint = "{endpoint}/api/embed"
model = "fixture:model"
model_revision = "fixture-revision"
dimension = 2
max_request_usd = 0.0
"#
    );
    fs::write(host.0.join("routes.toml"), &text).unwrap();
    text
}

fn command(host: &support::Host, action: &str, port: u16) -> Command {
    let mut cmd = host.command();
    cmd.env_clear()
        .env("PATH", "")
        .env("HOME", &host.0)
        .env("CPU_EMBED_NUM_GPU", "0")
        .args([
            "embedding-broker",
            action,
            "--port",
            &port.to_string(),
            "--timeout-seconds",
            "10",
            "--lifetime-seconds",
            "30",
            "--no-ollama-start",
            "--routes",
        ])
        .arg(host.0.join("routes.toml"))
        .arg("--state-dir")
        .arg(host.0.join("state"));
    cmd
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .new_agent()
}

fn decode(mut response: ureq::http::Response<ureq::Body>) -> (u16, Value, String) {
    let status = response.status().as_u16();
    let close = response
        .headers()
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(2 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap(), close)
}

fn get(port: u16, path: &str) -> (u16, Value, String) {
    decode(
        agent()
            .get(format!("http://127.0.0.1:{port}{path}"))
            .call()
            .unwrap(),
    )
}

fn post(port: u16, raw: &str, origin: bool) -> (u16, Value, String) {
    let mut request = agent()
        .post(format!("http://127.0.0.1:{port}/v1/embeddings"))
        .header("Content-Type", "application/json");
    if origin {
        request = request.header("Origin", "https://untrusted.invalid");
    }
    decode(request.send(raw).unwrap())
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}

fn start(mut cmd: Command, port: u16) -> Server {
    let mut server = Server(
        cmd.stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            let mut error = String::new();
            server
                .0
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut error)
                .unwrap();
            panic!("broker exited {status}: {error}");
        }
        if let Ok(response) = agent()
            .get(format!("http://127.0.0.1:{port}/health"))
            .call()
        {
            let (status, _, _) = decode(response);
            if status == 200 {
                return server;
            }
        }
        assert!(Instant::now() < deadline, "broker failed readiness");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn native_server_status_models_embeddings_and_warm_work_with_empty_path() {
    let host = support::Host::new();
    let (endpoint, worker) = fixture::server(vec![
        fixture::reply("/api/embed", json!({"embeddings":[[3,4]]})),
        fixture::reply("/api/embed", json!({"embeddings":[[3,4]]})),
    ]);
    let text = routes(&host, &endpoint);
    let port = port();
    let server = start(command(&host, "serve", port), port);
    let (status, health, close) = get(port, "/health");
    assert_eq!((status, close.as_str()), (200, "close"));
    assert_eq!(health["broker_pid"], server.0.id());
    let config = RoutingConfig::parse(&text).unwrap();
    let expected = config.routes[0]
        .resolve(&environment())
        .unwrap()
        .unwrap()
        .fingerprint();
    assert_eq!(health["route"]["fingerprint"], expected);
    assert_eq!(get(port, "/v1/models").1["data"][0]["id"], "fixture:model");
    let (status, embedded, _) = post(
        port,
        &json!({"input":"native","workspace_required_fingerprint":expected}).to_string(),
        false,
    );
    assert_eq!(status, 200);
    assert_eq!(embedded["data"][0]["embedding"], json!([0.6, 0.8]));
    assert_eq!(embedded["workspace_embedding"]["num_gpu"], 0);
    for action in ["status", "warm"] {
        let output = command(&host, action, port).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["ok"], true);
        if action == "warm" {
            assert_eq!(value["dimension"], 2);
        }
    }
    assert_eq!(worker.join().unwrap().len(), 2);
}

#[test]
fn malformed_requests_policy_changes_and_browser_origins_fail_before_backend() {
    let host = support::Host::new();
    let original = routes(&host, "http://127.0.0.1:1");
    let port = port();
    let _server = start(command(&host, "serve", port), port);
    for raw in [
        "{}",
        "[]",
        "{\"input\":[]}",
        "{\"input\":[true]}",
        "{\"input\":\"\"}",
        "{\"input\":\"a\",\"input\":\"b\"}",
        "{\"input\":\"a\",\"workspace_required_fingerprint\":3}",
    ] {
        assert_eq!(post(port, raw, false).0, 400, "{raw}");
    }
    assert_eq!(
        post(port, &json!({"input":vec!["x";129]}).to_string(), false).0,
        400
    );
    assert_eq!(post(port, "{\"input\":\"x\"}", true).0, 403);
    assert_eq!(get(port, "/v1/embeddings").0, 405);
    assert_eq!(get(port, "/missing").0, 404);
    fs::write(host.0.join("routes.toml"), "schema_version = -1").unwrap();
    assert_eq!(get(port, "/health").0, 503);
    assert_eq!(post(port, "{\"input\":\"x\"}", false).0, 503);
    fs::write(host.0.join("routes.toml"), original).unwrap();
    assert_eq!(get(port, "/health").0, 200);
}

#[test]
fn existing_mismatched_service_is_refused_without_creating_startup_state() {
    let host = support::Host::new();
    let original = routes(&host, "http://127.0.0.1:1");
    let port = port();
    let server = start(command(&host, "serve", port), port);
    let other = host.0.join("other.toml");
    fs::write(
        &other,
        original.replace("fixture-revision", "different-revision"),
    )
    .unwrap();
    let output = host
        .command()
        .env_clear()
        .env("PATH", "")
        .env("CPU_EMBED_NUM_GPU", "0")
        .args([
            "embedding-broker",
            "ensure",
            "--no-ollama-start",
            "--port",
            &port.to_string(),
            "--routes",
        ])
        .arg(other)
        .arg("--state-dir")
        .arg(host.0.join("state"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not match"));
    assert!(!host.0.join("state").exists());
    assert_eq!(get(port, "/health").1["broker_pid"], server.0.id());
}

struct Detached {
    address: SocketAddr,
    pid: u32,
    started: u64,
}
impl Drop for Detached {
    fn drop(&mut self) {
        if forge::process_listener::owned(self.address, self.pid).ok() == Some(self.started) {
            assert_eq!(unsafe { libc::kill(self.pid as i32, libc::SIGTERM) }, 0);
            let deadline = Instant::now() + Duration::from_secs(3);
            while std::net::TcpStream::connect_timeout(&self.address, Duration::from_millis(50))
                .is_ok()
            {
                assert!(Instant::now() < deadline, "owned broker did not stop");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[test]
fn concurrent_ensure_starts_exactly_one_owned_native_broker() {
    let host = support::Host::new();
    routes(&host, "http://127.0.0.1:1");
    let port = port();
    let children: Vec<_> = (0..4)
        .map(|_| {
            command(&host, "ensure", port)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut started = 0;
    let mut owned = None;
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        if value["started"] == true {
            started += 1;
            let pid = value["pid"].as_u64().unwrap() as u32;
            let address = ([127, 0, 0, 1], port).into();
            owned = Some(Detached {
                address,
                pid,
                started: forge::process_listener::owned(address, pid).unwrap(),
            });
        }
    }
    assert_eq!(started, 1);
    assert!(owned.is_some());
    assert_eq!(get(port, "/health").0, 200);
}

#[test]
fn finite_lifetime_and_slow_gpu_health_do_not_confuse_native_startup() {
    let host = support::Host::new();
    routes(&host, "http://127.0.0.1:1");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/broker_smi.rs");
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let compiled = Command::new(compiler)
        .arg(source)
        .arg("-o")
        .arg(host.0.join("nvidia-smi"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let port = port();
    let mut cmd = command(&host, "ensure", port);
    cmd.env_remove("CPU_EMBED_NUM_GPU").env("PATH", &host.0);
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let pid = report["pid"].as_u64().unwrap() as u32;
    let address = ([127, 0, 0, 1], port).into();
    let _owned = Detached {
        address,
        pid,
        started: forge::process_listener::owned(address, pid).unwrap(),
    };
    assert_eq!(get(port, "/health").1["num_gpu"], 99);
    let lifetime_port = crate::port();
    let mut cmd = host.command();
    cmd.env_clear()
        .env("PATH", "")
        .env("CPU_EMBED_NUM_GPU", "0")
        .args([
            "embedding-broker",
            "serve",
            "--port",
            &lifetime_port.to_string(),
            "--lifetime-seconds",
            "1",
            "--routes",
        ])
        .arg(host.0.join("routes.toml"))
        .arg("--no-ollama-start");
    let mut short = start(cmd, lifetime_port);
    let deadline = Instant::now() + Duration::from_secs(3);
    while short.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "finite broker lifetime exceeded");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn saturated_backend_workers_fail_fast_and_recover_capacity() {
    let host = support::Host::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    routes(&host, &format!("http://{}", listener.local_addr().unwrap()));
    let port = port();
    let _server = start(command(&host, "serve", port), port);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (arrived, waiting) = std::sync::mpsc::channel();
    let gate = release.clone();
    let worker = std::thread::spawn(move || blocked_backend(listener, arrived, gate));
    let clients: Vec<_> = (0..4)
        .map(|_| std::thread::spawn(move || post(port, "{\"input\":\"x\"}", false)))
        .collect();
    for _ in 0..4 {
        waiting.recv_timeout(Duration::from_secs(4)).unwrap();
    }
    let began = Instant::now();
    let busy = get(port, "/health");
    assert_eq!(busy.0, 503);
    assert!(began.elapsed() < Duration::from_secs(1));
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    for client in clients {
        assert_eq!(client.join().unwrap().0, 200);
    }
    worker.join().unwrap();
    assert_eq!(get(port, "/health").0, 200);
}

fn blocked_backend(
    listener: TcpListener,
    arrived: std::sync::mpsc::Sender<()>,
    gate: Arc<(Mutex<bool>, Condvar)>,
) {
    use std::{
        io::{BufReader, Write},
        os::fd::AsRawFd,
    };
    let mut workers = Vec::new();
    for _ in 0..4 {
        let mut poll = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert!(unsafe { libc::poll(&mut poll, 1, 4000) } > 0);
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let arrived = arrived.clone();
        let gate = gate.clone();
        workers.push(std::thread::spawn(move || {
            let mut input = BufReader::new(stream);
            fixture::read_request(&mut input);
            arrived.send(()).unwrap();
            let (released, _) = gate
                .1
                .wait_timeout_while(gate.0.lock().unwrap(), Duration::from_secs(4), |ready| {
                    !*ready
                })
                .unwrap();
            assert!(*released, "test failed to release backend requests");
            let body = "{\"embeddings\":[[3,4]]}";
            write!(
                input.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
}
