//! Length-bounded JSONL over a private Unix socket. No line is parsed or dispatched
//! before the complete frame arrives; duplicate call keys remain visible to validation.
use super::{state, Harness};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, value::RawValue, Value};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{atomic::Ordering, Arc};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{watch, Semaphore};

pub const MAX_FRAME: usize = 1024 * 1024;
const MAX_REPLY: usize = 2 * 1024 * 1024;

enum Request {
    Catalog,
    Grammar,
    Status,
    Shutdown,
    Call { call: Box<RawValue>, repair: bool },
    CallText { input: String, repair: bool },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Method {
    Catalog,
    Grammar,
    Status,
    Shutdown,
    Call,
    CallText,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    method: Method,
    call: Option<Box<RawValue>>,
    input: Option<String>,
    repair: Option<bool>,
}

fn parse_request(bytes: &[u8]) -> Result<Request> {
    // Internally tagged serde enums buffer fields and cannot preserve RawValue.
    // A direct struct visitor retains the original call including duplicate keys.
    let wire: Wire = serde_json::from_slice(bytes)?;
    match wire.method {
        Method::Call => {
            ensure!(wire.input.is_none(), "call does not accept input");
            Ok(Request::Call {
                call: wire.call.context("call is required")?,
                repair: wire.repair.unwrap_or(false),
            })
        }
        Method::CallText => {
            ensure!(wire.call.is_none(), "call_text does not accept call");
            Ok(Request::CallText {
                input: wire.input.context("input is required")?,
                repair: wire.repair.unwrap_or(false),
            })
        }
        method => {
            ensure!(
                wire.call.is_none() && wire.input.is_none() && wire.repair.is_none(),
                "method does not accept payload fields"
            );
            Ok(match method {
                Method::Catalog => Request::Catalog,
                Method::Grammar => Request::Grammar,
                Method::Status => Request::Status,
                Method::Shutdown => Request::Shutdown,
                _ => unreachable!(),
            })
        }
    }
}

struct Socket {
    path: PathBuf,
    dev: u64,
    ino: u64,
}
impl Drop for Socket {
    fn drop(&mut self) {
        if let Ok(meta) = std::fs::symlink_metadata(&self.path) {
            if meta.dev() == self.dev && meta.ino() == self.ino {
                if let Err(error) = std::fs::remove_file(&self.path) {
                    eprintln!("forge: socket cleanup failed: {error}");
                }
            }
        }
    }
}

pub async fn serve(harness: Harness, root: &Path, unit: &str) -> Result<()> {
    let path = root.join("socket");
    ensure!(
        path.as_os_str().as_encoded_bytes().len() < 108,
        "Unix socket path exceeds Linux limit; use a shorter state parent"
    );
    let listener = UnixListener::bind(&path)
        .context("bind new private harness socket (existing sockets are never removed)")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let meta = std::fs::symlink_metadata(&path)?;
    let _socket = Socket {
        path,
        dev: meta.dev(),
        ino: meta.ino(),
    };
    let harness = Arc::new(harness);
    let mut fatal = harness.fatal.clone();
    let (stop, mut stopped) = watch::channel(false);
    let slots = Arc::new(Semaphore::new(64));
    let mut clients = tokio::task::JoinSet::new();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    state::write_new(
        &root.join("ready.json"),
        &serde_json::to_vec(&json!({"pid":std::process::id(),"unit":unit,"protocol":1}))?,
    )?;
    loop {
        tokio::select! {
            _=terminate.recv()=>break, _=interrupt.recv()=>break, _=stopped.changed()=>break,
            _=fatal.changed()=>break,
            client=clients.join_next(),if !clients.is_empty()=>{if let Some(Err(error))=client {eprintln!("forge: connection worker failed: {error}");}},
            accepted=listener.accept()=>{
                let (stream,_)=accepted?;
                let Ok(permit)=slots.clone().try_acquire_owned() else {continue};
                let harness=harness.clone(); let stop=stop.clone();
                clients.spawn(async move {let _permit=permit;
                    if let Err(error)=connection(stream,harness,stop).await {eprintln!("forge: client closed: {error:#}");}
                });
            },
        }
    }
    harness.stopping.store(true, Ordering::Release);
    stop.send_replace(true);
    drop(listener);
    let mut failures = Vec::new();
    if let Some(outbound) = &harness.outbound {
        match tokio::time::timeout(Duration::from_secs(4), outbound.shutdown()).await {
            Ok(Ok(())) => (),
            Ok(Err(error)) => failures.push(format!("outbound shutdown: {error:#}")),
            Err(error) => failures.push(format!("outbound shutdown deadline exceeded: {error}")),
        }
    }
    match tokio::time::timeout(Duration::from_secs(4), harness.terminals.close_all()).await {
        Ok(Ok(())) => (),
        Ok(Err(error)) => failures.push(format!("terminal shutdown: {error:#}")),
        Err(error) => failures.push(format!("terminal shutdown deadline exceeded: {error}")),
    }
    match tokio::time::timeout(Duration::from_secs(2), async {
        while clients.join_next().await.is_some() {}
    })
    .await
    {
        Ok(()) => (),
        Err(error) => failures.push(format!("active client shutdown deadline exceeded: {error}")),
    }
    if let Some(error) = fatal.borrow().as_deref() {
        failures.push(format!("harness failed: {error}"));
    }
    ensure!(
        failures.is_empty(),
        "harness shutdown failed: {}",
        failures.join("; ")
    );
    Ok(())
}

async fn connection(
    stream: UnixStream,
    harness: Arc<Harness>,
    stop: watch::Sender<bool>,
) -> Result<()> {
    ensure!(
        stream.peer_cred()?.uid() == unsafe { libc::geteuid() },
        "peer UID mismatch"
    );
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::with_capacity(8192, read);
    let mut frame = Vec::new();
    let mut stopped = stop.subscribe();
    loop {
        if *stopped.borrow() {
            break;
        }
        let present = tokio::select! {
            _=stopped.changed()=>break,
            frame=tokio::time::timeout(Duration::from_secs(60),read_frame(&mut read,&mut frame,MAX_FRAME))=>frame.context("request deadline exceeded")??,
        };
        if !present {
            break;
        }
        let mut shutdown = false;
        let outcome = match parse_request(&frame) {
            Ok(Request::Catalog) => Ok(harness.catalog().clone()),
            Ok(Request::Grammar) => Ok(json!({"grammar":harness.grammar()})),
            Ok(Request::Status) => {
                let mut status = json!({"pid":std::process::id(),"protocol":1,"source_root":harness.policy.source_root,
                    "stopping":harness.stopping.load(Ordering::Acquire)});
                if let Some(outbound) = &harness.outbound {
                    status["outbound_mcp"] = outbound.status();
                }
                Ok(status)
            }
            Ok(Request::Shutdown) => {
                shutdown = true;
                Ok(json!({"stopping":true}))
            }
            Ok(Request::Call { call, repair }) => harness.call(call.get(), repair).await,
            Ok(Request::CallText { input, repair }) => harness.call(&input, repair).await,
            Err(error) => Err(error),
        };
        let reply = match outcome {
            Ok(result) => json!({"ok":true,"result":result}),
            Err(error) => json!({"ok":false,"error":format!("{error:#}")}),
        };
        let mut bytes = serde_json::to_vec(&reply)?;
        ensure!(bytes.len() <= MAX_REPLY, "response exceeds byte bound");
        bytes.push(b'\n');
        tokio::time::timeout(Duration::from_secs(3), write.write_all(&bytes))
            .await
            .context("response write deadline exceeded")??;
        if shutdown {
            stop.send_replace(true);
            break;
        }
    }
    Ok(())
}

pub async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    frame: &mut Vec<u8>,
    max: usize,
) -> Result<bool> {
    frame.clear();
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            ensure!(frame.is_empty(), "truncated JSONL frame");
            return Ok(false);
        }
        let newline = bytes.iter().position(|b| *b == b'\n');
        let count = newline.unwrap_or(bytes.len());
        ensure!(
            count <= max.saturating_sub(frame.len()),
            "frame exceeds byte bound"
        );
        frame.extend_from_slice(&bytes[..count]);
        reader.consume(count + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(true);
        }
    }
}

pub async fn request(socket: &Path, request: &[u8]) -> Result<Value> {
    ensure!(
        request.len() <= MAX_FRAME && !request.contains(&b'\n'),
        "client request must be one bounded JSON line"
    );
    let mut stream = UnixStream::connect(socket)
        .await
        .context("connect to native harness")?;
    ensure!(
        stream.peer_cred()?.uid() == unsafe { libc::geteuid() },
        "server UID mismatch"
    );
    stream.write_all(request).await?;
    stream.write_all(b"\n").await?;
    let mut reader = BufReader::new(stream);
    let mut frame = Vec::new();
    ensure!(
        read_frame(&mut reader, &mut frame, MAX_REPLY).await?,
        "server closed without a response"
    );
    Ok(serde_json::from_slice(&frame)?)
}

/// Validate an explicit host binding before exposing a service through MCP.
/// Same-UID service owners are trusted; this is not process attestation.
pub async fn verify_host(socket: &Path, root: &Path) -> Result<Value> {
    let reply = tokio::time::timeout(
        Duration::from_secs(3),
        request(socket, br#"{"method":"status"}"#),
    )
    .await
    .context("harness status deadline exceeded")??;
    ensure!(reply["ok"] == true, "harness status failed");
    let status = &reply["result"];
    ensure!(
        status["protocol"] == 1
            && status["stopping"] == false
            && status["pid"].as_u64().is_some_and(|pid| pid > 0),
        "harness is stopping or has an incompatible status"
    );
    ensure!(
        status["source_root"].as_str().map(Path::new) == Some(root),
        "harness source root differs from the requested host"
    );
    Ok(status.clone())
}
