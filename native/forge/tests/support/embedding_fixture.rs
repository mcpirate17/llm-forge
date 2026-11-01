#![allow(dead_code)]
use forge::embedding::Config;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::thread::JoinHandle;
use std::time::Duration;

pub struct Reply {
    pub path: &'static str,
    pub status: u16,
    pub body: Value,
    pub wait_for_close: bool,
    pub before_reply: Option<Box<dyn FnOnce() + Send>>,
}
pub fn reply(path: &'static str, body: Value) -> Reply {
    Reply {
        path,
        status: 200,
        body,
        wait_for_close: false,
        before_reply: None,
    }
}
pub fn inventory(digest: u8) -> Value {
    json!({"models":[{"name":"local-embed:test","digest":format!("{digest:02x}").repeat(32),"size":123456,"details":{}}]})
}
pub fn output() -> Value {
    json!({"model":"local-embed:test","embeddings":[[3.0,4.0]],"prompt_eval_count":8})
}
pub fn config(endpoint: String) -> Config {
    Config {
        endpoint,
        model: "local-embed:test".into(),
        runner: String::new(),
        artifact_sha256: [7; 32],
        dimension: 2,
        context_tokens: 2048,
        threads: 1,
        gpu_layers: 0,
        timeout_seconds: 3,
    }
}
pub type Requests = Vec<(String, Vec<u8>)>;
pub fn server(replies: Vec<Reply>) -> (String, JoinHandle<Requests>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let mut calls = Vec::new();
        for reply in replies {
            let mut wait = libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert!(
                unsafe { libc::poll(&mut wait, 1, 4000) } > 0,
                "expected request for {}",
                reply.path
            );
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut input = BufReader::new(stream);
            let (line, body) = read_request(&mut input);
            assert!(line.contains(reply.path), "unexpected request {line}");
            calls.push((line, body));
            if let Some(hook) = reply.before_reply {
                hook();
            }
            if reply.wait_for_close {
                let mut byte = [0u8];
                assert_eq!(input.read(&mut byte).unwrap(), 0);
                continue;
            }
            let body = reply.body.to_string();
            let sent = write!(input.get_mut(), "HTTP/1.1 {} Fixture\r\nContent-Length: {}\r\nContent-Type: application/json\r\nLocation: http://203.0.113.10:80/blocked\r\nConnection: close\r\n\r\n{}", reply.status, body.len(), body);
            if let Err(error) = sent {
                assert!(
                    body.len() > 1024 * 1024
                        && matches!(
                            error.kind(),
                            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                        ),
                    "response write: {error}"
                );
            }
        }
        calls
    });
    (endpoint, worker)
}
fn read_request(input: &mut BufReader<TcpStream>) -> (String, Vec<u8>) {
    let mut line = String::new();
    assert!(
        input.read_line(&mut line).unwrap() > 0,
        "request closed before start line"
    );
    let first = line.clone();
    let mut length = 0;
    for header in 0..=128 {
        line.clear();
        assert!(
            input.read_line(&mut line).unwrap() > 0,
            "request closed before headers completed"
        );
        assert!(line.len() <= 4096);
        if line == "\r\n" {
            break;
        }
        assert!(header < 128, "request header count exceeded");
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse::<usize>().unwrap();
        }
    }
    assert!(length <= 512 * 1024);
    let mut body = vec![0u8; length];
    input.read_exact(&mut body).unwrap();
    (first, body)
}
