//! Explicit, bounded model probes. Clerk requires an operator-designated dedicated server.
use super::{cell, dispatch};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn http(port: u16, path: &str, body: Option<&Value>, seconds: u64) -> Result<Value> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(seconds)))
        .proxy(None)
        .max_redirects(0)
        .max_response_header_size(16_384)
        .http_status_as_error(false)
        .build()
        .new_agent();
    let url = format!("http://127.0.0.1:{port}{path}");
    let mut response = if let Some(body) = body {
        let bytes = serde_json::to_vec(body)?;
        agent
            .post(&url)
            .header("Content-Type", "application/json")
            .send(bytes.as_slice())?
    } else {
        agent.get(&url).call()?
    };
    ensure!(
        response.status().as_u16() == 200,
        "endpoint returned HTTP {}",
        response.status()
    );
    let bytes = response
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_to_vec()?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn ps(port: u16) -> Result<(String, Value)> {
    let response = http(port, "/api/ps", None, 5)?;
    let models = response["models"]
        .as_array()
        .context("model inventory missing")?;
    ensure!(models.len() <= 128, "model inventory exceeds limit");
    let mut text = String::from("NAME ID SIZE PROCESSOR UNTIL\n");
    for model in models {
        let name = model["name"].as_str().context("model name missing")?;
        ensure!(
            name.len() <= 256 && !name.contains('\n'),
            "invalid model name"
        );
        let size = model["size"].as_u64().context("model size missing")?;
        let vram = model["size_vram"]
            .as_u64()
            .context("model GPU size missing")?;
        text.push_str(&format!(
            "{name} - {size} {} -\n",
            if vram > 0 { "GPU" } else { "CPU" }
        ));
    }
    Ok((text, response))
}
pub(super) fn embedding() -> Value {
    embedding_at(7317, 11434)
}
fn embedding_at(port: u16, inventory_port: u16) -> Value {
    let result = (|| -> Result<Value> {
        let health = http(port, "/health", None, 5)?;
        let response = http(
            port,
            "/v1/embeddings",
            Some(&json!({"model":"qwen3-embed-cpu","input":["workspace reliability canary"]})),
            120,
        )?;
        let vector = response["data"][0]["embedding"]
            .as_array()
            .context("embedding vector missing")?;
        let finite = vector
            .iter()
            .all(|v| v.as_f64().is_some_and(f64::is_finite));
        let (processes, _) = ps(inventory_port)?;
        dispatch(
            "check_embedding",
            &json!({"health":health,"processes":processes,"dimension":vector.len(),"finite":finite}),
        )
    })();
    result.unwrap_or_else(|error| {
        cell(
            "embedding-canary",
            "NOT_READY",
            &format!("embedding unavailable: {error:#}"),
            json!({}),
        )
    })
}

/// Confirm the selected same-UID PID owns the loopback listening socket.
fn listener_owned(port: u16, pid: u32) -> Result<u64> {
    forge::process_listener::owned(([127, 0, 0, 1], port).into(), pid)
}
fn gpu_preflight(root: &Path, smi: &Path, port: u16) -> Result<Value> {
    forge::repository::checkout_of_checked(root)?
        .context("GPU preflight requires a Git checkout")?;
    let (claims, _) = forge::ownership::load_claims(root)?;
    let now = forge::instant::now();
    let mut active = Vec::new();
    for claim in claims {
        if claim.active(now)? {
            active.push(json!({"claim_id":claim.claim_id,"owner":claim.owner,"paths":claim.paths,"justification":claim.justification}));
        }
    }
    let mut cmd = Command::new(smi);
    cmd.args([
        "--query-compute-apps=pid,process_name,used_memory",
        "--format=csv,noheader,nounits",
    ]);
    let out = forge::process_probe::run(&mut cmd, Duration::from_secs(5), 128 * 1024)?;
    ensure!(out.status.success(), "nvidia-smi inventory failed");
    let processes = dispatch(
        "parse_gpu_processes",
        &json!({"stdout":std::str::from_utf8(&out.stdout)?}),
    )?;
    let (loaded, _) = ps(port)?;
    dispatch(
        "gpu_preflight",
        &json!({"claims":active,"processes":processes,"ollama_ps":loaded}),
    )
}
fn check_server_identity(port: u16, pid: u32, started: u64) -> Result<()> {
    ensure!(
        listener_owned(port, pid)? == started,
        "dedicated server identity changed"
    );
    Ok(())
}
fn run_clerk(root: &Path, port: u16, pid: u32, smi: &Path) -> Result<Value> {
    let started = listener_owned(port, pid)?;
    let preflight = gpu_preflight(root, smi, port)?;
    ensure!(
        preflight["ready"] == true,
        "GPU ownership preflight is not clear: {preflight}"
    );
    let schema = dispatch("clerk_schema", &json!({}))?;
    let payload = dispatch(
        "clerk_payload",
        &json!({"schema":schema,"schema_text":serde_json::to_string(&schema)?,
        "system_prompt":"You are a bounded JSON-only clerical assistant."}),
    )?;
    check_server_identity(port, pid, started)?;
    ensure!(
        gpu_preflight(root, smi, port)?["ready"] == true,
        "GPU ownership changed before call"
    );
    let response = http(port, "/api/chat", Some(&payload), 180)?;
    check_server_identity(port, pid, started)?;
    let (resident, _) = ps(port)?;
    // This cleanup acts only on the operator-designated dedicated server, after
    // a second PID/socket identity check; it never targets the shared default.
    check_server_identity(port, pid, started)?;
    let _unload = http(
        port,
        "/api/generate",
        Some(&json!({"model":"qwen3.5:9b","keep_alive":0})),
        30,
    )?;
    let (after, _) = ps(port)?;
    let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&response)?));
    let verdict = dispatch(
        "clerk_adjudicate",
        &json!({"preflight":preflight,"response":response,
        "resident_processes":resident,"after_processes":after,"stop_returncode":0,"response_sha256":hash}),
    )?;
    Ok(cell(
        "local-clerk-canary",
        if verdict["ok"] == true {
            "PASS"
        } else {
            "FAIL-CLOSED"
        },
        "dedicated server clerk probe; exclusive server ownership is operator-attested",
        verdict["evidence"].clone(),
    ))
}
pub(super) fn clerk(
    root: &Path,
    port: Option<u16>,
    pid: Option<u32>,
    smi: Option<&Path>,
    exclusive: bool,
) -> Value {
    if !exclusive {
        return cell(
            "local-clerk-canary",
            "NOT_READY",
            "explicit --assert-exclusive-clerk-server is required",
            json!({}),
        );
    }
    let (Some(port), Some(pid), Some(smi)) = (port, pid, smi) else {
        return cell(
            "local-clerk-canary",
            "NOT_READY",
            "dedicated server port, server PID and nvidia-smi path are required",
            json!({}),
        );
    };
    if port == 11434 {
        return cell(
            "local-clerk-canary",
            "NOT_READY",
            "shared default Ollama port is not accepted",
            json!({}),
        );
    }
    run_clerk(root, port, pid, smi).unwrap_or_else(|error| {
        cell(
            "local-clerk-canary",
            "NOT_READY",
            &format!("clerk unavailable or preflight failed: {error:#}"),
            json!({"dedicated_server_pid":pid,"port":port,
                "cleanup":"not confirmed; an interrupted request may still be active"}),
        )
    })
}

#[cfg(test)]
#[path = "../tests/support/embedding_fixture.rs"]
mod embedding_fixture;

#[cfg(test)]
mod tests {
    use super::*;
    use embedding_fixture::{reply, Requests};
    use std::net::TcpListener;

    fn server(replies: Vec<embedding_fixture::Reply>) -> (u16, std::thread::JoinHandle<Requests>) {
        let (endpoint, worker) = embedding_fixture::server(replies);
        let socket: std::net::SocketAddr =
            endpoint.strip_prefix("http://").unwrap().parse().unwrap();
        (socket.port(), worker)
    }

    #[test]
    fn embedding_call_uses_fixture_and_finite_bound_vector_policy() {
        let (broker, a) = server(vec![
            reply(
                "/health",
                json!({"ok":true,"model":"qwen3-embed-cpu","num_ctx":2048,"num_gpu":0,"keep_alive":0}),
            ),
            reply(
                "/v1/embeddings",
                json!({"data":[{"embedding":vec![0.25;1024]}]}),
            ),
        ]);
        let (inventory, b) = server(vec![reply("/api/ps", json!({"models":[]}))]);
        let verdict = embedding_at(broker, inventory);
        assert_eq!(a.join().unwrap().len(), 2);
        assert_eq!(b.join().unwrap().len(), 1);
        assert_eq!(verdict["status"], "PASS");
    }

    #[test]
    fn listener_pid_must_own_the_loopback_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let started = listener_owned(port, std::process::id()).unwrap();
        assert!(check_server_identity(port, std::process::id(), started).is_ok());
        assert!(check_server_identity(port, std::process::id(), started + 1).is_err());
        assert!(listener_owned(port, 0).is_err());
        assert_eq!(
            clerk(
                Path::new("/"),
                Some(11434),
                Some(std::process::id()),
                Some(Path::new("/missing")),
                true
            )["status"],
            "NOT_READY"
        );
    }
}
