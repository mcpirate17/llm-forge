use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::thread;

fn accept_bounded(listener: &TcpListener) -> TcpStream {
    let mut poll = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let status = unsafe { libc::poll(&mut poll, 1, 4000) };
    assert!(
        status > 0 && poll.revents & libc::POLLIN != 0,
        "fixture request timed out"
    );
    let (stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
}

#[test]
fn rejects_invalid_vector_shapes_and_numbers() {
    for value in [json!([]), json!(["x", 1]), json!([true]), json!({})] {
        assert!(vector(&value).is_err());
    }
    assert_eq!(vector(&json!([3, 4])).unwrap(), vec![3.0, 4.0]);
}

#[test]
fn gpu_override_is_explicit_and_bounded() {
    let mut env = Env::new();
    for value in ["-1", "1", "abc"] {
        env.insert("CPU_EMBED_NUM_GPU".into(), value.into());
        assert!(device::choose_num_gpu(&env).is_err());
    }
    for value in ["0", "99"] {
        env.insert("CPU_EMBED_NUM_GPU".into(), value.into());
        assert_eq!(
            device::choose_num_gpu(&env).unwrap(),
            value.parse::<u32>().unwrap()
        );
    }
}

#[test]
fn transport_never_reports_secrets_or_server_body_and_disables_redirect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/api/embed", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let mut stream = accept_bounded(&listener);
        let mut request = [0; 4096];
        let _ = stream.read(&mut request).unwrap();
        stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/leak\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret").unwrap();
    });
    let error = transport::post_json(
        &endpoint,
        &json!({}),
        Some("my-secret"),
        Instant::now() + Duration::from_secs(2),
    )
    .unwrap_err()
    .to_string();
    worker.join().unwrap();
    assert!(error.contains("302"));
    assert!(!error.contains("secret") && !error.contains(&endpoint));
}

#[test]
fn elapsed_deadline_refuses_before_network() {
    let result = transport::post_json("http://127.0.0.1:1", &json!({}), None, Instant::now());
    assert!(result.unwrap_err().to_string().contains("deadline"));
}

fn route(endpoint: String) -> ResolvedRoute {
    ResolvedRoute {
        route_id: "local".into(),
        backend_kind: "ollama-local".into(),
        protocol: "ollama-embed".into(),
        priority: 1,
        normalization: "l2-client".into(),
        tokenizer_policy: "backend-default".into(),
        truncation_policy: "unicode-prefix".into(),
        max_input_chars: 2000,
        device_policy: "cpu".into(),
        paid: false,
        endpoint,
        api_key_env: "TEST_KEY".into(),
        model: "test:model".into(),
        model_revision: "rev".into(),
        dimension: 2,
        cost_per_million_tokens: 0.0,
        max_request_usd: 0.0,
    }
}

fn fixture(body: Value) -> (String, thread::JoinHandle<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/embed", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let mut stream = accept_bounded(&listener);
        let mut request = Vec::new();
        let mut chunk = [0; 4096];
        let header_end = loop {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&chunk[..n]);
            assert!(request.len() <= 16 * 1024, "fixture header exceeds 16 KiB");
            if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let length: usize = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse().unwrap())
            })
            .unwrap();
        assert!(length <= 1024 * 1024, "fixture body exceeds 1 MiB");
        while request.len() < header_end + length {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&chunk[..n]);
        }
        let data = serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
        let response = body.to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        )
        .unwrap();
        data
    });
    (url, worker)
}

#[test]
fn local_request_clips_unicode_and_preserves_resource_controls() {
    let (endpoint, worker) = fixture(json!({"embeddings":[[3.0,4.0]]}));
    let route = route(endpoint);
    let rows = ollama(
        &route,
        &["🦀".repeat(2001)],
        99,
        Instant::now() + Duration::from_secs(3),
    )
    .unwrap();
    let normalized = validate_vectors(rows, 1, &route).unwrap();
    assert_eq!(normalized[0], [0.6, 0.8]);
    let request = worker.join().unwrap();
    assert_eq!(request["input"][0].as_str().unwrap().chars().count(), 2000);
    assert_eq!(
        request["options"],
        json!({"num_gpu":99,"num_ctx":2048,"num_thread":8})
    );
    assert_eq!(request["keep_alive"], 0);
}

#[test]
fn paid_vectors_are_ordered_and_duplicate_indexes_rejected() {
    let (endpoint, worker) =
        fixture(json!({"data":[{"index":1,"embedding":[0,1]},{"index":0,"embedding":[1,0]}]}));
    let mut env = Env::new();
    env.insert("TEST_KEY".into(), "secret".into());
    let selected = route(endpoint);
    let rows = paid(
        &selected,
        &["a".into(), "b".into()],
        &env,
        Instant::now() + Duration::from_secs(3),
    )
    .unwrap();
    assert_eq!(rows, [vec![1.0, 0.0], vec![0.0, 1.0]]);
    worker.join().unwrap();
    let (endpoint, worker) =
        fixture(json!({"data":[{"index":0,"embedding":[1,0]},{"index":0,"embedding":[0,1]}]}));
    assert!(paid(
        &route(endpoint),
        &["a".into(), "b".into()],
        &env,
        Instant::now() + Duration::from_secs(3)
    )
    .is_err());
    worker.join().unwrap();
}

fn two_local_routes(fallback_endpoint: String) -> (RoutingConfig, Env) {
    let mut config = RoutingConfig::load(None).unwrap();
    let mut primary = config.routes[0].clone();
    primary.endpoint = "http://127.0.0.1:1/embed".into();
    primary.dimension = 2;
    let mut fallback = primary.clone();
    fallback.route_id = "fallback".into();
    fallback.endpoint = fallback_endpoint;
    config.primary_route = primary.route_id.clone();
    config.fallback_routes = vec![fallback.route_id.clone()];
    config.routes = vec![primary, fallback];
    let mut env = Env::new();
    env.insert("CPU_EMBED_NUM_GPU".into(), "0".into());
    (config, env)
}

#[test]
fn route_fallback_works_and_pin_disallows_switching() {
    let (endpoint, worker) = fixture(json!({"embeddings":[[3.0,4.0]]}));
    let (config, env) = two_local_routes(endpoint);
    let options = BackendOptions {
        timeout: Duration::from_secs(3),
        allow_ollama_start: false,
    };
    let result = embed(
        &["hello".into()],
        "",
        &config,
        &QualityReceipt::default(),
        &env,
        &options,
    )
    .unwrap();
    assert_eq!(result["data"][0]["embedding"], json!([0.6, 0.8]));
    assert_eq!(
        result["workspace_embedding"]["reserved_cost_usd"],
        json!(0.0)
    );
    assert_eq!(
        result["workspace_embedding"]["request_budget_usd"],
        json!(0.0)
    );
    assert_eq!(
        result["workspace_embedding"]["selection_reason"],
        "availability-fallback"
    );
    worker.join().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (config, env) =
        two_local_routes(format!("http://{}/embed", listener.local_addr().unwrap()));
    let pin = config.routes[0]
        .resolve(&env)
        .unwrap()
        .unwrap()
        .fingerprint();
    assert!(embed(
        &["hello".into()],
        &pin,
        &config,
        &QualityReceipt::default(),
        &env,
        &options
    )
    .is_err());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn paid_cost_limit_refuses_before_network() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (mut config, mut env) =
        two_local_routes(format!("http://{}/embed", listener.local_addr().unwrap()));
    let mut paid = config.routes[1].clone();
    paid.paid = true;
    paid.protocol = "openai-embeddings".into();
    paid.api_key_env = "TEST_KEY".into();
    paid.cost_per_million_tokens_env = "TEST_COST".into();
    paid.max_request_usd = 0.0;
    config.primary_route = paid.route_id.clone();
    config.fallback_routes.clear();
    config.routes = vec![paid];
    env.insert("TEST_KEY".into(), "secret".into());
    env.insert("TEST_COST".into(), "1".into());
    let options = BackendOptions {
        timeout: Duration::from_secs(1),
        allow_ollama_start: false,
    };
    let error = embed(
        &["hello".into()],
        "",
        &config,
        &QualityReceipt::default(),
        &env,
        &options,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("cost limit"));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn normalization_none_preserves_zero_and_large_finite_vectors() {
    let mut route = route("http://127.0.0.1:1/embed".into());
    let vectors = vec![vec![0.0, 0.0], vec![f64::MAX, -f64::MAX]];
    route.normalization = "none".into();
    assert_eq!(
        validate_vectors(vectors.clone(), 2, &route).unwrap(),
        vectors
    );
    route.normalization = "l2-client".into();
    for vector in vectors {
        assert!(validate_vectors(vec![vector], 1, &route).is_err());
    }
}

#[test]
fn duplicate_response_keys_are_rejected_without_echoing_payload() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/embed", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let mut stream = accept_bounded(&listener);
        let mut request = [0; 4096];
        let _ = stream.read(&mut request).unwrap();
        let body = r#"{"data":[{"index":0,"index":1,"embedding":[1,0]}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let error = transport::post_json(
        &endpoint,
        &json!({}),
        None,
        Instant::now() + Duration::from_secs(2),
    )
    .unwrap_err()
    .to_string();
    worker.join().unwrap();
    assert_eq!(error, "embedding backend returned invalid JSON");
}

#[test]
fn request_budget_reserves_failed_paid_attempts_and_obeys_individual_caps() {
    let mut budget = Reservation {
        ceiling: 0.05,
        reserved: 0.0,
    };
    assert!(budget.admit(true, 0.04, 0.05));
    assert!(!budget.admit(true, 0.04, 0.05));
    assert_eq!(budget.reserved, 0.04);
    assert!(budget.admit(false, 0.0, 0.0));
    assert_eq!(budget.reserved, 0.04);
    let mut budget = Reservation {
        ceiling: 0.10,
        reserved: 0.0,
    };
    assert!(!budget.admit(true, 0.04, 0.03));
    assert_eq!(budget.reserved, 0.0);
    assert!(budget.admit(true, 0.04, 0.10));
    assert!(budget.admit(true, 0.05, 0.05));
    assert!(!budget.admit(true, 0.02, 0.10));
}

#[test]
fn request_budget_is_largest_eligible_paid_cap_and_zero_for_local_only() {
    let (mut config, mut env) = two_local_routes("http://127.0.0.1:2/embed".into());
    let local = quality::route_attempts(&config, &QualityReceipt::default(), "", &env).unwrap();
    let budget = Reservation::new(&local);
    assert_eq!((budget.ceiling, budget.reserved), (0.0, 0.0));
    for (i, route) in config.routes.iter_mut().enumerate() {
        route.paid = true;
        route.protocol = "openai-embeddings".into();
        route.api_key_env = "TEST_KEY".into();
        route.max_request_usd = if i == 0 { 0.05 } else { 0.10 };
    }
    env.insert("TEST_KEY".into(), "secret".into());
    let paid = quality::route_attempts(&config, &QualityReceipt::default(), "", &env).unwrap();
    assert_eq!(Reservation::new(&paid).ceiling, 0.10);
}
