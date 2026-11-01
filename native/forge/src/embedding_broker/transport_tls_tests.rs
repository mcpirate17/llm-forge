use super::*;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::io::Write;
use std::net::TcpListener;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

const CA: &[u8] = include_bytes!("../../tests/fixtures/embedding_broker/ca.der");
const CERT: &[u8] = include_bytes!("../../tests/fixtures/embedding_broker/server.der");
const KEY: &[u8] = include_bytes!("../../tests/fixtures/embedding_broker/server-key.der");
const SECRET: &str = "public-test-bearer-canary";

// Exactly one connection, bounded accept/IO deadlines, no detached fixture thread.
fn server() -> (String, JoinHandle<Vec<u8>>) {
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(CERT.to_vec())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY.to_vec())),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("https://{}/embed", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "TLS client never connected");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("TLS accept failed: {error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut tls = rustls::StreamOwned::new(connection, socket);
        let mut received = Vec::new();
        let mut buffer = [0; 2048];
        loop {
            match tls.read(&mut buffer) {
                Ok(0) | Err(_) => return received,
                Ok(count) => received.extend_from_slice(&buffer[..count]),
            }
            assert!(received.len() <= 8192, "oversized fixture request");
            if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&received[..end]).unwrap();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse().unwrap())
                    })
                    .unwrap();
                if received.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let response = br#"{"encrypted":true}"#;
        write!(tls, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
        tls.write_all(response).unwrap();
        tls.flush().unwrap();
        received
    });
    (endpoint, worker)
}

#[test]
fn encrypted_post_verifies_test_ca_and_delivers_bearer_and_json() {
    let (endpoint, worker) = server();
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::new_with_certs(&[
            ureq::tls::Certificate::from_der(CA),
        ]))
        .build();
    let client = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .tls_config(tls)
        .build()
        .new_agent();
    let body = serde_json::json!({"input":["private fixture input"]});
    let result = post_with_agent(
        &client,
        &endpoint,
        &body,
        Some(SECRET),
        Instant::now() + Duration::from_secs(3),
    );
    let received = worker.join().unwrap();
    assert_eq!(result.unwrap(), serde_json::json!({"encrypted":true}));
    let end = received.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let headers = std::str::from_utf8(&received[..end])
        .unwrap()
        .to_ascii_lowercase();
    assert!(headers.starts_with("post /embed http/1.1\r\n"));
    assert!(headers.contains(&format!("authorization: bearer {SECRET}")));
    assert_eq!(
        serde_json::from_slice::<Value>(&received[end + 4..]).unwrap(),
        body
    );
}

#[test]
fn production_webpki_client_refuses_test_ca_before_authorization() {
    let (endpoint, worker) = server();
    let result = post_json(
        &endpoint,
        &serde_json::json!({"input":["never sent"]}),
        Some(SECRET),
        Instant::now() + Duration::from_secs(3),
    );
    let received = worker.join().unwrap();
    let error = result.unwrap_err().to_string();
    assert_eq!(error, "embedding backend request failed");
    assert!(!error.contains(SECRET) && !error.contains(&endpoint));
    assert!(
        received.is_empty(),
        "untrusted TLS peer received application bytes"
    );
}
