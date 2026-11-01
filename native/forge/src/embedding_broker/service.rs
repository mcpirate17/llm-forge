//! Bounded HTTP admission with blocking backend work kept off the event loop.
use super::{
    backend::{self, BackendOptions},
    config::Env,
    device, Settings,
};
use anyhow::{ensure, Context, Result};
use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[path = "../mailbox_server_io.rs"]
#[allow(dead_code)]
mod bounded_io;

const MAX_BODY: usize = 1024 * 1024;
const MAX_REPLY: usize = 32 * 1024 * 1024;

#[derive(Clone)]
struct Shared {
    settings: Settings,
    env: Arc<Env>,
    active: Arc<Semaphore>,
}

fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({"error":{"message":message,"type":"embedding_route_error"}})),
    )
        .into_response()
}

pub fn health(settings: &Settings, env: &Env) -> Result<Value> {
    let (attempts, quality) = settings.attempts(env)?;
    let primary = attempts.first().context("no approved embedding route")?;
    let gpu = if primary.route.protocol == "ollama-embed" {
        Some(device::choose_num_gpu(env)?)
    } else {
        None
    };
    Ok(
        json!({"ok":true,"broker_pid":std::process::id(),"num_gpu":gpu,"num_ctx":2048,"keep_alive":0,
        "model":primary.route.model,"route":primary.route.public_metadata(),
        "selection_reason":primary.reason.as_str(),"quality_receipt_sha256":quality.sha256}),
    )
}

fn models(settings: &Settings, env: &Env) -> Result<Value> {
    let (attempts, _) = settings.attempts(env)?;
    Ok(
        json!({"object":"list","data": attempts.iter().map(|attempt| json!({
        "id":attempt.route.model,"object":"model","owned_by":attempt.route.backend_kind,
        "route_id":attempt.route.route_id,"paid":attempt.route.paid
    })).collect::<Vec<_>>()}),
    )
}

fn input(payload: &Value) -> Result<(Vec<String>, &str)> {
    ensure!(payload.is_object(), "request body must be a JSON object");
    let texts = match payload.get("input") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(rows)) if rows.len() <= 128 => rows
            .iter()
            .map(|row| {
                row.as_str()
                    .map(str::to_owned)
                    .context("input entries must be strings")
            })
            .collect::<Result<Vec<_>>>()?,
        _ => anyhow::bail!("input must be a string or 1..128 strings"),
    };
    ensure!(
        !texts.is_empty() && texts.iter().all(|s| !s.is_empty() && !s.contains('\0')),
        "input strings must be nonempty and contain no NUL"
    );
    let pin = payload
        .get("workspace_required_fingerprint")
        .map(|v| {
            v.as_str()
                .context("workspace_required_fingerprint must be a string")
        })
        .transpose()?
        .unwrap_or("");
    ensure!(pin.len() <= 71, "invalid required fingerprint");
    Ok((texts, pin))
}

struct Failure(StatusCode, String);

fn policy_error(reason: anyhow::Error) -> Failure {
    Failure(StatusCode::SERVICE_UNAVAILABLE, reason.to_string())
}

fn request_error(reason: anyhow::Error) -> Failure {
    Failure(StatusCode::BAD_REQUEST, reason.to_string())
}

fn execute(shared: &Shared, path: &str, payload: Option<Value>) -> Result<Value, Failure> {
    match path {
        "/health" => health(&shared.settings, &shared.env).map_err(policy_error),
        "/v1/models" => models(&shared.settings, &shared.env).map_err(policy_error),
        "/v1/embeddings" => {
            let payload = payload
                .context("missing embedding request")
                .map_err(request_error)?;
            let (texts, pin) = input(&payload).map_err(request_error)?;
            let (config, quality) = shared.settings.policy(&shared.env).map_err(policy_error)?;
            backend::embed(
                &texts,
                pin,
                &config,
                &quality,
                &shared.env,
                &BackendOptions {
                    timeout: shared.settings.timeout,
                    allow_ollama_start: shared.settings.allow_ollama_start,
                },
            )
            .map_err(request_error)
        }
        _ => Err(Failure(StatusCode::NOT_FOUND, "unknown endpoint".into())),
    }
}

async fn body(request: Request<Body>) -> Result<Value> {
    let content_type = request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    ensure!(
        content_type.split(';').next() == Some("application/json"),
        "Content-Type must be application/json"
    );
    let bytes = tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(request.into_body(), MAX_BODY),
    )
    .await
    .context("request body deadline expired")?
    .context("request body exceeds 1 MiB or cannot be read")?;
    crate::tool_schema::parse_document(std::str::from_utf8(&bytes)?)
}

async fn dispatch(State(shared): State<Shared>, request: Request<Body>) -> Response {
    let mut response = dispatch_inner(shared, request).await;
    // Each accepted connection has one absolute first-header deadline. Closing
    // after its response prevents a keep-alive request bypassing that deadline.
    response
        .headers_mut()
        .insert("connection", axum::http::HeaderValue::from_static("close"));
    response
}

async fn dispatch_inner(shared: Shared, request: Request<Body>) -> Response {
    if request.headers().contains_key("origin") {
        return error(
            StatusCode::FORBIDDEN,
            "browser-origin requests are not supported",
        );
    }
    let method = match request.uri().path() {
        "/health" | "/v1/models" => axum::http::Method::GET,
        "/v1/embeddings" => axum::http::Method::POST,
        _ => return error(StatusCode::NOT_FOUND, "unknown endpoint"),
    };
    if request.method() != method {
        return error(StatusCode::METHOD_NOT_ALLOWED, "unsupported method");
    }
    let permit = match shared.active.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "embedding broker is busy"),
    };
    let path = request.uri().path().to_owned();
    let payload = if path == "/v1/embeddings" {
        match body(request).await {
            Ok(value) => Some(value),
            Err(reason) => return error(StatusCode::BAD_REQUEST, &reason.to_string()),
        }
    } else {
        None
    };
    run_blocking(shared, path, payload, permit).await
}

async fn run_blocking(
    shared: Shared,
    path: String,
    payload: Option<Value>,
    permit: OwnedSemaphorePermit,
) -> Response {
    let health_request = path == "/health";
    let timeout = shared.settings.timeout + Duration::from_secs(5);
    let work = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let value = execute(&shared, &path, payload)?;
        let bytes = serde_json::to_vec(&value).map_err(|_| {
            Failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot encode embedding response".into(),
            )
        })?;
        if bytes.len() > MAX_REPLY {
            return Err(Failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "embedding response exceeds 32 MiB".into(),
            ));
        }
        Ok::<_, Failure>(bytes)
    });
    match tokio::time::timeout(timeout, work).await {
        Ok(Ok(Ok(bytes))) => ([("content-type", "application/json")], bytes).into_response(),
        Ok(Ok(Err(reason))) if health_request => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok":false,"error":reason.1})),
        )
            .into_response(),
        Ok(Ok(Err(reason))) => error(reason.0, &reason.1),
        _ => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "embedding worker failed or deadline expired",
        ),
    }
}

pub fn serve(settings: Settings, env: Env) -> Result<()> {
    settings.validate()?;
    settings.attempts(&env)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(4)
        .build()?;
    let shutdown_timeout = settings.timeout + Duration::from_secs(5);
    let result = runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(settings.bind).await?;
        let lifetime = settings.lifetime;
        let bounded = bounded_io::BoundedListener::with_limits(listener, 16, shutdown_timeout);
        let shared = Shared {
            settings,
            env: Arc::new(env),
            active: Arc::new(Semaphore::new(4)),
        };
        let app = Router::new()
            .route("/health", any(dispatch))
            .route("/v1/models", any(dispatch))
            .route("/v1/embeddings", any(dispatch))
            .fallback(dispatch)
            .with_state(shared);
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let lifespan = async {
            match lifetime {
                Some(duration) => tokio::time::sleep(duration).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            result = async { axum::serve(bounded, app).await } => result?,
            _ = lifespan => (),
            _ = term.recv() => (),
            _ = tokio::signal::ctrl_c() => (),
        }
        Ok::<_, anyhow::Error>(())
    });
    runtime.shutdown_timeout(shutdown_timeout);
    result
}
