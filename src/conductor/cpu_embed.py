#!/usr/bin/env python3
"""Provider-neutral embedding broker with bounded, observable routes.

Ollama's ``/v1/embeddings`` ignores ``num_gpu`` and inherited
``OLLAMA_CONTEXT_LENGTH=131072``, which loaded a 0.6B embedder into ~6 GB VRAM.
This loopback process is the canonical embedding endpoint for workspace
consumers.  It prefers the local GPU-guest route and can use an explicitly
enabled paid route when a measured quality receipt fails or the local route is
unavailable.  Every response identifies the exact vector-space fingerprint.
"""

from __future__ import annotations

import argparse
import fcntl
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final, Iterator

import uvicorn
from starlette.applications import Starlette
from starlette.requests import Request
from starlette.responses import JSONResponse
from starlette.routing import Route

from conductor.embedding_contract import (
    EmbeddingContractError,
    QualityReceipt,
    ResolvedRoute,
    RouteAttempt,
    RoutingConfig,
    load_quality_receipt,
    load_routing_config,
    route_attempts,
)
from conductor.http_transport import open_http

BIND_HOST: Final[str] = "127.0.0.1"
BIND_PORT: Final[int] = 7317
BROKER_HEALTH_URL: Final[str] = f"http://{BIND_HOST}:{BIND_PORT}/health"
OLLAMA_HEALTH_URL: Final[str] = "http://127.0.0.1:11434/api/tags"
OLLAMA_EMBED_URL: Final[str] = "http://127.0.0.1:11434/api/embed"
DEFAULT_MODEL: Final[str] = "qwen3-embed-cpu"
FALLBACK_MODEL: Final[str] = "qwen3-embedding:0.6b-q8_0"
NUM_CTX: Final[int] = 2048
NUM_THREAD: Final[int] = 8
MAX_CHARS: Final[int] = 2000
OLLAMA_BATCH: Final[int] = 16
KEEP_ALIVE_DONE: Final[int] = 0
GPU_BUSY_MIB: Final[int] = 2048
ALLOWED_NUM_GPU: Final[frozenset[int]] = frozenset({0, 99})
_SKIP_GPU_HOLDERS: Final[tuple[str, ...]] = (
    "gnome-remote-desktop",
    "Xorg",
    "gnome-shell",
    "ollama",
)
STARTUP_TIMEOUT_SECONDS: Final[float] = 20.0


class CpuEmbedError(RuntimeError):
    """Fail-closed embedding proxy error."""


@dataclass(frozen=True, slots=True)
class RoutedEmbedding:
    """Vectors plus the route decision needed for index provenance."""

    vectors: list[list[float]]
    attempt: RouteAttempt
    quality_receipt_sha256: str
    estimated_cost_usd: float
    num_gpu: int | None


def _runtime_lock_path(name: str) -> Path:
    root = Path(os.environ.get("XDG_RUNTIME_DIR", tempfile.gettempdir()))
    return root / f"llm-workspace-{os.getuid()}-{name}.lock"


@contextmanager
def _startup_lock(name: str) -> Iterator[None]:
    path = _runtime_lock_path(name)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a+", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def _read_json_url(url: str, *, timeout: float) -> dict[str, Any]:
    with open_http(
        urllib.request.urlopen, url, timeout=timeout, error_type=CpuEmbedError
    ) as response:
        payload = json.loads(response.read().decode("utf-8"))
    if not isinstance(payload, dict):
        raise CpuEmbedError(f"health response from {url!r} is not an object")
    return payload


def _wait_for_health(
    url: str,
    *,
    timeout: float,
    validator: Any | None = None,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            payload = _read_json_url(url, timeout=0.5)
            if validator is None or validator(payload):
                return payload
        except (OSError, TimeoutError, ValueError, urllib.error.URLError) as exc:
            last_error = exc
        time.sleep(0.05)
    raise CpuEmbedError(
        f"service at {url!r} did not become ready in {timeout:.1f}s: {last_error}"
    )


def ensure_ollama_service(timeout: float = STARTUP_TIMEOUT_SECONDS) -> bool:
    """Ensure the loopback Ollama daemon exists; return whether this call started it."""

    try:
        _read_json_url(OLLAMA_HEALTH_URL, timeout=0.5)
        return False
    except (OSError, TimeoutError, ValueError, urllib.error.URLError):
        pass
    with _startup_lock("ollama"):
        try:
            _read_json_url(OLLAMA_HEALTH_URL, timeout=0.5)
            return False
        except (OSError, TimeoutError, ValueError, urllib.error.URLError):
            pass
        executable = shutil.which("ollama")
        if executable is None:
            raise CpuEmbedError("ollama executable is missing")
        subprocess.Popen(  # noqa: S603 - resolved executable and fixed argv
            [executable, "serve"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        _wait_for_health(OLLAMA_HEALTH_URL, timeout=timeout)
        return True


def ensure_service(timeout: float = STARTUP_TIMEOUT_SECONDS) -> bool:
    """Ensure the canonical broker is reachable; return whether this call started it."""

    try:
        expected = route_attempts(
            load_routing_config(), quality=load_quality_receipt()
        )[0].route
    except EmbeddingContractError as exc:
        raise CpuEmbedError(str(exc)) from exc

    def valid(payload: dict[str, Any]) -> bool:
        route = payload.get("route")
        return (
            payload.get("ok") is True
            and isinstance(route, dict)
            and route.get("fingerprint") == expected.fingerprint
        )

    def check_existing() -> bool:
        payload = _read_json_url(BROKER_HEALTH_URL, timeout=0.5)
        if valid(payload):
            return True
        route = payload.get("route")
        actual = route.get("fingerprint") if isinstance(route, dict) else None
        raise CpuEmbedError(
            "running embedding broker route does not match this process: "
            f"actual={actual!r}, expected={expected.fingerprint!r}; restart the "
            "broker after applying provider or quality-policy environment changes"
        )

    try:
        if check_existing():
            return False
    except (OSError, TimeoutError, ValueError, urllib.error.URLError):
        pass
    with _startup_lock("embedding-broker"):
        try:
            if check_existing():
                return False
        except (OSError, TimeoutError, ValueError, urllib.error.URLError):
            pass
        if expected.protocol == "ollama-embed":
            ensure_ollama_service(timeout=timeout)
        subprocess.Popen(  # noqa: S603 - current interpreter and fixed module argv
            [sys.executable, "-m", "conductor.cpu_embed", "serve"],
            cwd=Path(__file__).resolve().parents[1],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        _wait_for_health(BROKER_HEALTH_URL, timeout=timeout, validator=valid)
        return True


def _as_text_list(value: Any) -> list[str]:
    if isinstance(value, str):
        if value:
            return [value]
        raise CpuEmbedError("input string must not be empty")
    if (
        value
        and isinstance(value, list)
        and all(isinstance(item, str) and item for item in value)
    ):
        return value
    raise CpuEmbedError(
        "input must be a non-empty string or array of non-empty strings"
    )


def choose_num_gpu() -> int:
    """99 when the 5090 is free enough; 0 if a research job is holding it."""
    force = os.environ.get("CPU_EMBED_NUM_GPU")
    if force is not None:
        try:
            forced = int(force)
        except ValueError as exc:
            raise CpuEmbedError(
                f"CPU_EMBED_NUM_GPU must be 0 or 99, got {force!r}"
            ) from exc
        if forced not in ALLOWED_NUM_GPU:
            raise CpuEmbedError(f"CPU_EMBED_NUM_GPU must be 0 or 99, got {forced}")
        return forced
    try:
        raw = subprocess.check_output(
            [
                "nvidia-smi",
                "--query-compute-apps=process_name,used_memory",
                "--format=csv,noheader,nounits",
            ],
            text=True,
            timeout=2,
        )
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return 0
    busy = 0
    for line in raw.splitlines():
        parts = [p.strip() for p in line.split(",")]
        if len(parts) < 2:
            continue
        name, mem = parts[0], parts[1]
        if any(skip in name for skip in _SKIP_GPU_HOLDERS):
            continue
        try:
            busy += int(mem)
        except ValueError:
            continue
    return 0 if busy >= GPU_BUSY_MIB else 99


def pin_options(num_gpu: int | None = None) -> dict[str, int]:
    selected = choose_num_gpu() if num_gpu is None else num_gpu
    if selected not in ALLOWED_NUM_GPU:
        raise CpuEmbedError(f"num_gpu must be 0 or 99, got {selected}")
    return {
        "num_gpu": selected,
        "num_ctx": NUM_CTX,
        "num_thread": NUM_THREAD,
    }


def ollama_embed(
    texts: list[str],
    *,
    model: str,
    url: str = OLLAMA_EMBED_URL,
    timeout_s: float = 120.0,
    num_gpu: int | None = None,
) -> list[list[float]]:
    if not texts:
        return []
    clipped = [text[:MAX_CHARS] for text in texts]
    out: list[list[float]] = []
    for start in range(0, len(clipped), OLLAMA_BATCH):
        batch = clipped[start : start + OLLAMA_BATCH]
        last = start + OLLAMA_BATCH >= len(clipped)
        payload = {
            "model": model,
            "input": batch,
            "options": pin_options(num_gpu),
            "keep_alive": KEEP_ALIVE_DONE if last else "5s",
        }
        request = urllib.request.Request(
            url,
            data=json.dumps(payload).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with open_http(
                urllib.request.urlopen,
                request,
                timeout=timeout_s,
                error_type=CpuEmbedError,
            ) as response:
                body = json.loads(response.read().decode("utf-8"))
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
            raise CpuEmbedError(f"ollama embed failed: {exc}") from exc
        vectors = body.get("embeddings")
        if not isinstance(vectors, list) or len(vectors) != len(batch):
            raise CpuEmbedError("ollama embed returned the wrong vector count")
        for index, vector in enumerate(vectors):
            if not isinstance(vector, list) or not vector:
                raise CpuEmbedError(
                    f"ollama embedding {start + index} is not a non-empty array"
                )
            if not all(
                isinstance(value, (int, float))
                and not isinstance(value, bool)
                and math.isfinite(float(value))
                for value in vector
            ):
                raise CpuEmbedError(
                    f"ollama embedding {start + index} contains a non-finite or "
                    "non-numeric value"
                )
            out.append([float(value) for value in vector])
    return out


def _l2_normalize(vector: list[float], *, route_id: str) -> list[float]:
    norm = math.sqrt(sum(value * value for value in vector))
    if not math.isfinite(norm) or norm == 0.0:
        raise CpuEmbedError(f"route {route_id!r} returned a zero or non-finite vector")
    return [value / norm for value in vector]


def _validate_route_vectors(
    vectors: list[list[float]], route: ResolvedRoute, expected_count: int
) -> list[list[float]]:
    if len(vectors) != expected_count:
        raise CpuEmbedError(
            f"route {route.route_id!r} returned {len(vectors)} vectors; "
            f"expected {expected_count}"
        )
    normalized: list[list[float]] = []
    for index, vector in enumerate(vectors):
        if len(vector) != route.dimension:
            raise CpuEmbedError(
                f"route {route.route_id!r} vector {index} dimension "
                f"{len(vector)} != {route.dimension}"
            )
        if not all(math.isfinite(value) for value in vector):
            raise CpuEmbedError(
                f"route {route.route_id!r} vector {index} contains non-finite values"
            )
        normalized.append(
            _l2_normalize(vector, route_id=route.route_id)
            if route.normalization == "l2-client"
            else vector
        )
    return normalized


def _post_json(
    url: str,
    payload: dict[str, Any],
    *,
    headers: dict[str, str] | None = None,
    timeout_s: float,
) -> dict[str, Any]:
    request = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"} | (headers or {}),
        method="POST",
    )
    try:
        with open_http(
            urllib.request.urlopen,
            request,
            timeout=timeout_s,
            error_type=CpuEmbedError,
        ) as response:
            body = json.loads(response.read().decode("utf-8"))
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
        raise CpuEmbedError(f"embedding request to {url!r} failed: {exc}") from exc
    if not isinstance(body, dict):
        raise CpuEmbedError(f"embedding response from {url!r} is not an object")
    return body


def openai_compatible_embed(
    texts: list[str],
    *,
    route: ResolvedRoute,
    timeout_s: float = 120.0,
) -> list[list[float]]:
    """Call an explicitly enabled paid provider using a compatibility protocol."""

    api_key = os.environ.get(route.api_key_env, "") if route.api_key_env else ""
    if not api_key:
        raise CpuEmbedError(
            f"paid route {route.route_id!r} is missing credential env "
            f"{route.api_key_env!r}"
        )
    body = _post_json(
        route.endpoint,
        {"model": route.model, "input": texts},
        headers={"Authorization": f"Bearer {api_key}"},
        timeout_s=timeout_s,
    )
    data = body.get("data")
    if not isinstance(data, list):
        raise CpuEmbedError(f"paid route {route.route_id!r} response lacks data")
    ordered: list[tuple[int, list[float]]] = []
    for position, row in enumerate(data):
        if not isinstance(row, dict) or not isinstance(row.get("embedding"), list):
            raise CpuEmbedError(
                f"paid route {route.route_id!r} row {position} lacks an embedding"
            )
        index = row.get("index", position)
        if isinstance(index, bool) or not isinstance(index, int):
            raise CpuEmbedError(
                f"paid route {route.route_id!r} row {position} has invalid index"
            )
        vector = row["embedding"]
        if not all(
            isinstance(value, (int, float)) and not isinstance(value, bool)
            for value in vector
        ):
            raise CpuEmbedError(
                f"paid route {route.route_id!r} row {position} is not numeric"
            )
        ordered.append((index, [float(value) for value in vector]))
    ordered.sort(key=lambda item: item[0])
    indexes = [index for index, _vector in ordered]
    if indexes != list(range(len(texts))):
        raise CpuEmbedError(
            f"paid route {route.route_id!r} returned invalid indexes {indexes}"
        )
    return [vector for _, vector in ordered]


def embed_with_routing(
    texts: list[str],
    *,
    required_fingerprint: str = "",
    timeout_s: float = 120.0,
    config: RoutingConfig | None = None,
    quality: QualityReceipt | None = None,
) -> RoutedEmbedding:
    """Embed through the allowed route order and record why fallback occurred."""

    if not texts:
        raise CpuEmbedError("embedding input must not be empty")
    config = config or load_routing_config()
    quality = quality or load_quality_receipt()
    try:
        attempts = route_attempts(
            config,
            quality=quality,
            required_fingerprint=required_fingerprint,
        )
    except EmbeddingContractError as exc:
        raise CpuEmbedError(str(exc)) from exc
    failures: list[str] = []
    for attempt in attempts:
        route = attempt.route
        clipped = [text[: route.max_input_chars] for text in texts]
        estimated_cost = route.estimate_request_cost(clipped)
        if route.paid and estimated_cost > route.max_request_usd:
            failures.append(
                f"{route.route_id}: estimated cost ${estimated_cost:.6f} exceeds "
                f"${route.max_request_usd:.6f}"
            )
            continue
        try:
            if route.protocol == "ollama-embed":
                num_gpu = choose_num_gpu()
                try:
                    vectors = ollama_embed(
                        clipped,
                        model=route.model,
                        url=route.endpoint,
                        timeout_s=timeout_s,
                        num_gpu=num_gpu,
                    )
                except CpuEmbedError:
                    ensure_ollama_service()
                    num_gpu = choose_num_gpu()
                    vectors = ollama_embed(
                        clipped,
                        model=route.model,
                        url=route.endpoint,
                        timeout_s=timeout_s,
                        num_gpu=num_gpu,
                    )
            elif route.protocol == "openai-embeddings":
                vectors = openai_compatible_embed(
                    clipped, route=route, timeout_s=timeout_s
                )
                num_gpu = None
            else:
                raise CpuEmbedError(
                    f"route {route.route_id!r} uses unsupported protocol "
                    f"{route.protocol!r}"
                )
            validated = _validate_route_vectors(vectors, route, len(clipped))
            return RoutedEmbedding(
                vectors=validated,
                attempt=attempt,
                quality_receipt_sha256=quality.sha256,
                estimated_cost_usd=estimated_cost,
                num_gpu=num_gpu,
            )
        except CpuEmbedError as exc:
            failures.append(f"{route.route_id}: {exc}")
            if required_fingerprint:
                break
    raise CpuEmbedError("all approved embedding routes failed: " + "; ".join(failures))


def openai_embedding_response(
    model: str,
    vectors: list[list[float]],
    *,
    routed: RoutedEmbedding | None = None,
) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "object": "list",
        "model": model,
        "data": [
            {"object": "embedding", "index": i, "embedding": vec}
            for i, vec in enumerate(vectors)
        ],
        "usage": {"prompt_tokens": 0, "total_tokens": 0},
    }
    if routed is not None:
        payload["workspace_embedding"] = {
            **routed.attempt.route.public_metadata(),
            "selection_reason": routed.attempt.reason.value,
            "quality_receipt_sha256": routed.quality_receipt_sha256,
            "estimated_cost_usd": routed.estimated_cost_usd,
            "num_gpu": routed.num_gpu,
            "num_ctx": NUM_CTX
            if routed.attempt.route.protocol == "ollama-embed"
            else None,
            "keep_alive": KEEP_ALIVE_DONE
            if routed.attempt.route.protocol == "ollama-embed"
            else None,
        }
    return payload


def resolve_model(_requested: str | None = None) -> str:
    """Compatibility model name for legacy clients and health checks."""
    try:
        attempts = route_attempts(load_routing_config(), quality=load_quality_receipt())
    except EmbeddingContractError:
        return os.environ.get("CPU_EMBED_MODEL", DEFAULT_MODEL)
    return attempts[0].route.model


async def embeddings_route(request: Request) -> JSONResponse:
    try:
        payload = await request.json()
        if not isinstance(payload, dict):
            raise CpuEmbedError("request body must be a JSON object")
        texts = _as_text_list(payload.get("input"))
        required_fingerprint = payload.get("workspace_required_fingerprint", "")
        if not isinstance(required_fingerprint, str):
            raise CpuEmbedError("workspace_required_fingerprint must be a string")
        routed = embed_with_routing(texts, required_fingerprint=required_fingerprint)
        return JSONResponse(
            openai_embedding_response(
                routed.attempt.route.model,
                routed.vectors,
                routed=routed,
            )
        )
    except (CpuEmbedError, EmbeddingContractError, json.JSONDecodeError) as exc:
        return JSONResponse(
            {"error": {"message": str(exc), "type": "embedding_route_error"}},
            status_code=503 if isinstance(exc, EmbeddingContractError) else 400,
        )


async def models_route(_: Request) -> JSONResponse:
    config = load_routing_config()
    quality = load_quality_receipt()
    attempts = route_attempts(config, quality=quality)
    return JSONResponse(
        {
            "object": "list",
            "data": [
                {
                    "id": attempt.route.model,
                    "object": "model",
                    "owned_by": attempt.route.backend_kind,
                    "route_id": attempt.route.route_id,
                    "paid": attempt.route.paid,
                }
                for attempt in attempts
            ],
        }
    )


async def health_route(_: Request) -> JSONResponse:
    try:
        quality = load_quality_receipt()
        attempts = route_attempts(load_routing_config(), quality=quality)
        primary = attempts[0]
        num_gpu = choose_num_gpu() if primary.route.protocol == "ollama-embed" else None
    except (CpuEmbedError, EmbeddingContractError) as exc:
        return JSONResponse(
            {"ok": False, "error": str(exc), "model": resolve_model(None)},
            status_code=503,
        )
    return JSONResponse(
        {
            "ok": True,
            "num_gpu": num_gpu,
            "num_ctx": NUM_CTX,
            "keep_alive": KEEP_ALIVE_DONE,
            "model": primary.route.model,
            "route": primary.route.public_metadata(),
            "selection_reason": primary.reason.value,
            "quality_receipt_sha256": quality.sha256,
        }
    )


def build_app() -> Starlette:
    return Starlette(
        routes=[
            Route("/v1/embeddings", embeddings_route, methods=["POST"]),
            Route("/v1/models", models_route, methods=["GET"]),
            Route("/health", health_route, methods=["GET"]),
        ]
    )


def serve(host: str = BIND_HOST, port: int = BIND_PORT) -> None:
    uvicorn.run(build_app(), host=host, port=port, log_level="warning")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Provider-neutral embedding broker")
    parser.add_argument("command", choices=["serve", "ensure", "status"])
    parser.add_argument("--host", default=BIND_HOST)
    parser.add_argument("--port", type=int, default=BIND_PORT)
    args = parser.parse_args(argv)
    if args.command == "serve":
        serve(args.host, args.port)
        return 0
    if args.command == "ensure":
        started = ensure_service()
        print(json.dumps({"ok": True, "started": started, "health": BROKER_HEALTH_URL}))
        return 0
    if args.command == "status":
        try:
            print(json.dumps(_read_json_url(BROKER_HEALTH_URL, timeout=1.0), indent=2))
            return 0
        except (
            CpuEmbedError,
            OSError,
            TimeoutError,
            ValueError,
            urllib.error.URLError,
        ) as exc:
            print(json.dumps({"ok": False, "error": str(exc)}))
            return 2
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
