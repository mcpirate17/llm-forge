//! Concurrent native MCP stdio adapter for an already authorized harness.
//! Disconnecting the adapter leaves persistent sessions under their service owner.
#[path = "harness_mcp_protocol.rs"]
mod protocol;
use super::transport;
use anyhow::{ensure, Context, Result};
use protocol::{Action, Protocol};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

const MAX_REPLY: usize = 4 * 1024 * 1024;

#[cfg(test)]
#[path = "harness_mcp_cancellation_tests.rs"]
mod cancellation_tests;

pub async fn serve<R, W>(socket: PathBuf, mut input: R, mut output: W) -> Result<()>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let catalog = tokio::time::timeout(
        Duration::from_secs(3),
        transport::request(&socket, br#"{"method":"catalog"}"#),
    )
    .await
    .context("harness catalog deadline exceeded")??;
    ensure!(catalog["ok"] == true, "harness catalog failed: {catalog}");
    let mut protocol = Protocol::new(catalog["result"].clone())?;
    let (send, mut receive) = mpsc::channel(4);
    let reader = tokio::spawn(async move {
        loop {
            let mut bytes = Vec::new();
            let frame =
                match transport::read_frame(&mut input, &mut bytes, transport::MAX_FRAME).await {
                    Ok(true) => Ok(Some(bytes)),
                    Ok(false) => Ok(None),
                    Err(error) => Err(error),
                };
            let ended = !matches!(frame, Ok(Some(_)));
            if send.send(frame).await.is_err() || ended {
                break;
            }
        }
    });
    let mut pending = BTreeMap::<String, bool>::new();
    let mut cancellation = BTreeMap::<String, oneshot::Sender<()>>::new();
    let mut workers = tokio::task::JoinSet::new();
    let result=async {
        loop {
            tokio::select! {
                frame=receive.recv()=>{
                    let Some(frame)=frame else {break};let Some(frame)=frame? else {break};
                    match protocol.prepare(&frame,&pending) {
                        Action::Ignore=>{},
                        Action::Cancel(id)=>{
                            if let Some(cancelled)=pending.get_mut(&id) {*cancelled=true;}
                            if let Some(cancel)=cancellation.remove(&id) {let _=cancel.send(());}
                        },
                        Action::Reply(value)=>write_reply(&mut output,&value).await?,
                        Action::Forward{id,era,bytes,cancel_on_disconnect}=>{
                            let key=id.to_string();
                            if pending.contains_key(&key) {write_reply(&mut output,&protocol::error_reply(id,-32600,"Request id is already in flight",None)).await?;continue;}
                            if pending.len()>=16 {write_reply(&mut output,&protocol::error_reply(id,-32000,"Native adapter request capacity exhausted",None)).await?;continue;}
                            pending.insert(key.clone(),false);let socket=socket.clone();
                            let cancelled=if cancel_on_disconnect {
                                let (send,receive)=oneshot::channel();cancellation.insert(key.clone(),send);Some(receive)
                            } else {None};
                            workers.spawn(async move {(key,forwarded(id,era,&socket,&bytes,cancelled).await)});
                        },
                    }
                },
                done=workers.join_next(),if !workers.is_empty()=>{
                    let (key,value)=done.context("missing native MCP worker")?.context("native MCP worker panicked")?;
                    cancellation.remove(&key);
                    if pending.remove(&key)==Some(false) {if let Some(value)=value {write_reply(&mut output,&value).await?;}}
                },
            }
        }
        Ok(())
    }.await;
    reader.abort();
    let _ = reader.await;
    workers.abort_all();
    while workers.join_next().await.is_some() {}
    result
}

async fn forwarded(
    id: Value,
    era: protocol::Era,
    socket: &Path,
    bytes: &[u8],
    cancelled: Option<oneshot::Receiver<()>>,
) -> Option<Value> {
    let result = if let Some(cancelled) = cancelled {
        tokio::select! {
            biased;
            _ = cancelled => return None,
            result = forward(socket, bytes) => result,
        }
    } else {
        forward(socket, bytes).await
    };
    Some(protocol::tool_result(id, era, result))
}

async fn forward(socket: &Path, bytes: &[u8]) -> std::result::Result<Value, String> {
    let reply=tokio::time::timeout(Duration::from_secs(35),transport::request(socket,bytes)).await
        .map_err(|_|"Tool response deadline exceeded; execution may have started. Reuse the same session id/write sequence.".to_owned())?
        .map_err(|error|format!("Tool transport failed; execution may have started: {error:#}"))?;
    if reply["ok"] == true {
        Ok(reply["result"].clone())
    } else {
        Err(reply["error"]
            .as_str()
            .unwrap_or("Invalid harness response")
            .into())
    }
}

async fn write_reply(output: &mut (impl AsyncWrite + Unpin), value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_REPLY, "MCP response exceeds byte bound");
    bytes.push(b'\n');
    tokio::time::timeout(Duration::from_secs(3), output.write_all(&bytes))
        .await
        .context("MCP stdout backpressure deadline exceeded")??;
    Ok(())
}

pub async fn stdio(socket: PathBuf, source_root: Option<PathBuf>) -> Result<()> {
    if let Some(root) = source_root {
        let root = root.canonicalize().context("resolve bound source root")?;
        transport::verify_host(&socket, &root).await?;
    }
    let (input, _input_flags) = standard(libc::STDIN_FILENO)?;
    let (output, _output_flags) = standard(libc::STDOUT_FILENO)?;
    serve(socket, BufReader::new(input), output).await
}

fn standard(
    fd: i32,
) -> Result<(
    crate::process_io::Descriptor,
    crate::process_io::StandardStream,
)> {
    let guard = crate::process_io::StandardStream::new(fd)?;
    let descriptor = crate::process_io::Descriptor::new(guard.try_clone()?)
        .context("stdio descriptor must support epoll (use pipes or a terminal)")?;
    Ok((descriptor, guard))
}
