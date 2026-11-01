use super::*;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "forge-clerk-generate-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn source(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, text).unwrap();
        path
    }
    fn settings(&self) -> Settings {
        Settings::new(&self.0, &[]).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
struct HttpFixture {
    endpoint: String,
    worker: Option<JoinHandle<Vec<u8>>>,
}
impl HttpFixture {
    fn finish(mut self) -> Vec<u8> {
        self.worker.take().unwrap().join().unwrap()
    }
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                result.unwrap();
            }
        }
    }
}
fn server(reply: Vec<u8>, delay: Duration) -> HttpFixture {
    server_with_callback(reply, delay, || {})
}
fn server_with_callback(
    reply: Vec<u8>,
    delay: Duration,
    before_reply: impl FnOnce() + Send + 'static,
) -> HttpFixture {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "clerk fixture was never contacted"
                    );
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("clerk fixture accept: {error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = socket.read(&mut buffer).unwrap();
            assert!(count > 0, "incomplete clerk fixture request");
            request.extend_from_slice(&buffer[..count]);
            assert!(request.len() <= MAX_REQUEST_BYTES + 16_384);
            if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&request[..end]).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        before_reply();
        thread::sleep(delay);
        // A deadline/refused response may close the client before publication.
        let _ = socket.write_all(&reply);
        request
    });
    HttpFixture {
        endpoint,
        worker: Some(worker),
    }
}
fn response(status: &str, body: &[u8], declared: Option<usize>) -> Vec<u8> {
    let mut reply=format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        declared.unwrap_or(body.len())).into_bytes();
    reply.extend_from_slice(body);
    reply
}
fn model() -> Value {
    json!({"summary":"facts","source_decisions":[],"open_tasks":[],
        "todo_items":[],"duplicate_candidates":[]})
}
fn options(endpoint: &str) -> GenerateOptions {
    GenerateOptions {
        model: MODEL.into(),
        task_class: "compaction".into(),
        endpoint: endpoint.into(),
        timeout: Duration::from_secs(2),
    }
}
fn payload(raw: String) -> Vec<u8> {
    serde_json::to_vec(&json!({"model":MODEL,"response":raw,"eval_count":12})).unwrap()
}

#[test]
fn successful_draft_sends_fixed_request_and_has_verifiable_provenance() {
    let fixture = Fixture::new();
    let paths: Vec<_> = (0..3)
        .map(|n| {
            fixture.source(
                &format!("{n}.md"),
                &format!("# Source {n}\n{}", "界 facts ".repeat(600)),
            )
        })
        .collect();
    let body = payload(format!("```json\n{}\n```", model()));
    let http = server(response("200 OK", &body, None), Duration::ZERO);
    let generated = draft(&fixture.settings(), &paths, &options(&http.endpoint)).unwrap();
    super::super::validate_value(&fixture.settings(), &generated).unwrap();
    assert_eq!(generated["authority"], "local-clerk-only");
    assert_eq!(generated["eval_count"], 12);
    assert_eq!(generated["draft"], model());
    let bytes = http.finish();
    let end = bytes.windows(4).position(|v| v == b"\r\n\r\n").unwrap();
    let headers = std::str::from_utf8(&bytes[..end])
        .unwrap()
        .to_ascii_lowercase();
    assert!(headers.starts_with("post /api/generate http/1.1"));
    assert!(!headers.contains("authorization:"));
    let sent: Value = serde_json::from_slice(&bytes[end + 4..]).unwrap();
    assert_eq!(sent["model"], MODEL);
    assert_eq!(sent["system"], crate::local_ai_policy::CLERK_SYSTEM_PROMPT);
    assert_eq!(
        sent["options"],
        json!({"num_ctx":2048,"num_predict":384,"temperature":0.1})
    );
    for (key, value) in [
        ("stream", json!(false)),
        ("format", json!("json")),
        ("think", json!(false)),
        ("keep_alive", json!(-1)),
    ] {
        assert_eq!(sent[key], value);
    }
    let prompt = sent["prompt"].as_str().unwrap();
    assert!(prompt.chars().count() <= MAX_PROMPT_CHARS);
    let envelope: Value = serde_json::from_str(&prompt[prompt.find('{').unwrap()..]).unwrap();
    for (entry, metadata) in envelope["sources"]
        .as_array()
        .unwrap()
        .iter()
        .zip(generated["sources"].as_array().unwrap())
    {
        let count = entry["text"].as_str().unwrap().chars().count();
        assert!(count >= 128);
        assert_eq!(metadata["prompt_text_chars"], json!(count));
    }
}

#[test]
fn plain_origins_are_pinned_and_ambient_or_alias_urls_are_refused_privately() {
    assert_eq!(
        endpoint("http://localhost:11434/").unwrap(),
        "http://127.0.0.1:11434/api/generate"
    );
    assert_eq!(
        endpoint("http://[::1]:11434").unwrap(),
        "http://[::1]:11434/api/generate"
    );
    for raw in [
        "http://127.0.0.1/?secret-canary",
        "http://secret-canary@localhost",
        "https://localhost",
        "http://localhost/api/generate",
        "http://localhost#secret-canary",
        "http://127.1",
        "http://2130706433",
        "http://127.0.0.1.evil.invalid",
        "http://localhost:0",
        "http://localhost:",
        "http://[::1%25lo]",
        "http://localhost\\evil",
    ] {
        let error = endpoint(raw).unwrap_err().to_string();
        assert!(!error.contains("secret-canary"), "{error}");
    }
    let fixture = Fixture::new();
    let path = fixture.source("note.md", "facts");
    let body = payload(model().to_string());
    let http = server(response("200 OK", &body, None), Duration::ZERO);
    let alias = http.endpoint.replace("127.0.0.1", "localhost");
    draft(&fixture.settings(), &[path], &options(&alias)).unwrap();
    let request = http.finish();
    assert!(std::str::from_utf8(&request)
        .unwrap()
        .contains("127.0.0.1:"));
}

#[test]
fn wrappers_are_bounded_and_duplicate_keys_or_invalid_shapes_never_repair() {
    for raw in [
        model().to_string(),
        format!("```json\n{}\n```", model()),
        format!("Draft follows:\n{}\nreview required", model()),
    ] {
        assert_eq!(model_json(&raw).unwrap(), model());
    }
    let braces = json!({"summary":r#"a } brace \" quote"#,"source_decisions":[],
        "open_tasks":[],"todo_items":[],"duplicate_candidates":[]});
    assert_eq!(model_json(&format!("Text: {braces}")).unwrap(), braces);
    let duplicate = r#"{"summary":"a","summary":"b","source_decisions":[],"open_tasks":[],"todo_items":[],"duplicate_candidates":[]}"#;
    for raw in [
        duplicate.to_owned(),
        format!("wrapper {duplicate}"),
        "{}".into(),
        format!("[{}]", model()),
        "```json".into(),
        "{incomplete".into(),
        " ".repeat(MAX_RESPONSE_BYTES + 1),
    ] {
        assert!(model_json(&raw).is_err());
    }
}

#[test]
fn http_status_truncation_oversize_and_invalid_utf8_fail_without_server_body() {
    let fixture = Fixture::new();
    let source = fixture.source("note.md", "facts");
    let cases = vec![
        response("500 Failed", b"server-body-canary", None),
        response("200 OK", b"{}", Some(100)),
        response("200 OK", &vec![b' '; MAX_RESPONSE_BYTES + 1], None),
        response("200 OK", &[255], None),
        response(
            "200 OK",
            b"{\"response\":\"one\",\"response\":\"two\"}",
            None,
        ),
    ];
    for reply in cases {
        let http = server(reply, Duration::ZERO);
        let error = draft(
            &fixture.settings(),
            std::slice::from_ref(&source),
            &options(&http.endpoint),
        )
        .unwrap_err()
        .to_string();
        assert!(!error.contains("server-body-canary"));
        http.finish();
    }
}

#[test]
fn http_deadline_and_option_admission_are_finite() {
    let fixture = Fixture::new();
    let source = fixture.source("note.md", "facts");
    let body = payload(model().to_string());
    let http = server(response("200 OK", &body, None), Duration::from_millis(500));
    let mut selected = options(&http.endpoint);
    selected.timeout = Duration::from_millis(250);
    let started = Instant::now();
    assert!(draft(&fixture.settings(), &[source], &selected).is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
    http.finish();
    for timeout in [Duration::ZERO, Duration::from_secs(121)] {
        selected.timeout = timeout;
        assert!(selected.admitted_endpoint().is_err());
    }
    selected = options("http://127.0.0.1");
    selected.model = "other".into();
    assert!(selected.admitted_endpoint().is_err());
    selected.model = MODEL.into();
    selected.task_class = "approval".into();
    assert!(selected.admitted_endpoint().is_err());
}

#[test]
fn prompt_refuses_metadata_that_starves_source_coverage() {
    let fixture = Fixture::new();
    let source = fixture.source("note.md", &"facts ".repeat(600));
    let mut sources = read_sources(&fixture.settings(), &[source]).unwrap();
    sources[0].title = "long title ".repeat(500);
    assert!(prompt(&sources)
        .unwrap_err()
        .to_string()
        .contains("too little prompt room"));
    assert!(prompt(&[]).is_err());
}

#[test]
fn malformed_eval_counts_and_response_shapes_are_refused() {
    let fixture = Fixture::new();
    let source = fixture.source("note.md", "facts");
    let cases = [
        json!({"model":MODEL,"response":model().to_string(),"eval_count":true}),
        json!({"model":MODEL,"response":model().to_string(),"eval_count":-1}),
        json!({"model":MODEL,"response":model().to_string(),"eval_count":"12"}),
        json!({"model":MODEL,"response":false}),
        json!({"model":MODEL,"response":"{}"}),
    ];
    for value in cases {
        let bytes = serde_json::to_vec(&value).unwrap();
        let http = server(response("200 OK", &bytes, None), Duration::ZERO);
        assert!(draft(
            &fixture.settings(),
            std::slice::from_ref(&source),
            &options(&http.endpoint)
        )
        .is_err());
        http.finish();
    }
}

#[test]
fn redirect_status_is_returned_without_following_location() {
    let fixture = Fixture::new();
    let source = fixture.source("note.md", "facts");
    let reply = b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/never\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    let http = server(reply, Duration::ZERO);
    let error = draft(&fixture.settings(), &[source], &options(&http.endpoint)).unwrap_err();
    assert!(error.to_string().contains("HTTP 302"));
    http.finish();
}

#[test]
fn missing_mismatched_and_nonstring_server_model_labels_are_refused() {
    let fixture = Fixture::new();
    let source = fixture.source("note.md", "facts");
    let missing = json!({"response":model().to_string(),"eval_count":12});
    let mut mismatched = missing.clone();
    mismatched["model"] = json!("other-model");
    let mut nonstring = missing.clone();
    nonstring["model"] = json!(true);
    for value in [missing, mismatched, nonstring] {
        let body = serde_json::to_vec(&value).unwrap();
        let http = server(response("200 OK", &body, None), Duration::ZERO);
        let error = draft(
            &fixture.settings(),
            std::slice::from_ref(&source),
            &options(&http.endpoint),
        )
        .unwrap_err();
        assert!(error.to_string().contains("model label"));
        http.finish();
    }
}

#[test]
fn source_replacement_during_inference_refuses_document_return() {
    let fixture = Fixture::new();
    let source = fixture.source("note.md", &"original facts ".repeat(5000));
    let replacement = fixture.source("replacement.md", &"different facts ".repeat(5000));
    let path = source.clone();
    let body = payload(model().to_string());
    let http = server_with_callback(response("200 OK", &body, None), Duration::ZERO, move || {
        fs::rename(replacement, path).unwrap();
    });
    let error = draft(&fixture.settings(), &[source], &options(&http.endpoint)).unwrap_err();
    assert!(error.to_string().contains("source changed since draft"));
    http.finish();
}
