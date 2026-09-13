from __future__ import annotations

import json
from pathlib import Path

import pytest
from starlette.testclient import TestClient

from conductor import cpu_embed
from conductor import embedding_contract


def _route(**overrides: object) -> embedding_contract.ResolvedRoute:
    fields: dict[str, object] = {
        "route_id": "test-route",
        "backend_kind": "ollama-local",
        "protocol": "ollama-embed",
        "priority": 10,
        "normalization": "none",
        "tokenizer_policy": "backend-default",
        "truncation_policy": "unicode-prefix",
        "max_input_chars": 2000,
        "device_policy": "cpu",
        "paid": False,
        "endpoint": "http://127.0.0.1:11434/api/embed",
        "api_key_env": "",
        "model": "test-model",
        "model_revision": "sha256:" + "a" * 64,
        "dimension": 2,
        "cost_per_million_tokens": 0.0,
        "max_request_usd": 0.0,
    }
    fields.update(overrides)
    return embedding_contract.ResolvedRoute(**fields)


def _attempt(
    route: embedding_contract.ResolvedRoute,
    reason: embedding_contract.SelectionReason = embedding_contract.SelectionReason.PRIMARY,
) -> embedding_contract.RouteAttempt:
    return embedding_contract.RouteAttempt(route=route, reason=reason)


def _pin_route(
    monkeypatch: pytest.MonkeyPatch, route: embedding_contract.ResolvedRoute
) -> None:
    monkeypatch.setattr(
        cpu_embed, "route_attempts", lambda *_args, **_kwargs: (_attempt(route),)
    )


def _fail_route_attempts(monkeypatch: pytest.MonkeyPatch, message: str) -> None:
    def raise_contract_error(*_args: object, **_kwargs: object) -> None:
        raise embedding_contract.EmbeddingContractError(message)

    monkeypatch.setattr(cpu_embed, "route_attempts", raise_contract_error)


def _refuse_urlopen(monkeypatch: pytest.MonkeyPatch) -> None:
    def raise_url_error(*_args: object, **_kwargs: object) -> None:
        raise cpu_embed.urllib.error.URLError("connection refused")

    monkeypatch.setattr(cpu_embed.urllib.request, "urlopen", raise_url_error)


def test_pin_options_are_bounded(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CPU_EMBED_NUM_GPU", "0")
    opts = cpu_embed.pin_options()
    assert opts["num_gpu"] == 0
    assert opts["num_ctx"] == 2048
    monkeypatch.setenv("CPU_EMBED_NUM_GPU", "99")
    assert cpu_embed.pin_options()["num_gpu"] == 99
    monkeypatch.setenv("CPU_EMBED_NUM_GPU", "8")
    with pytest.raises(cpu_embed.CpuEmbedError, match="must be 0 or 99"):
        cpu_embed.pin_options()


def test_choose_num_gpu_returns_zero_when_nvidia_smi_unavailable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("CPU_EMBED_NUM_GPU", raising=False)
    monkeypatch.setattr(
        cpu_embed.subprocess,
        "check_output",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(OSError("no nvidia-smi")),
    )
    assert cpu_embed.choose_num_gpu() == 0


@pytest.mark.parametrize(
    "busy_mib", [cpu_embed.GPU_BUSY_MIB, cpu_embed.GPU_BUSY_MIB + 1]
)
def test_choose_num_gpu_returns_zero_at_busy_threshold(
    busy_mib: int, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("CPU_EMBED_NUM_GPU", raising=False)
    monkeypatch.setattr(
        cpu_embed.subprocess,
        "check_output",
        lambda *_args, **_kwargs: f"python, {busy_mib}\n",
    )
    assert cpu_embed.choose_num_gpu() == 0


def test_choose_num_gpu_returns_ninety_nine_when_gpu_free(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("CPU_EMBED_NUM_GPU", raising=False)
    monkeypatch.setattr(
        cpu_embed.subprocess,
        "check_output",
        lambda *_args, **_kwargs: f"python, {cpu_embed.GPU_BUSY_MIB - 1}\n",
    )
    assert cpu_embed.choose_num_gpu() == 99


def test_choose_num_gpu_ignores_skipped_holders(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("CPU_EMBED_NUM_GPU", raising=False)
    csv = f"Xorg, {cpu_embed.GPU_BUSY_MIB * 10}\npython, {cpu_embed.GPU_BUSY_MIB - 1}\n"
    monkeypatch.setattr(
        cpu_embed.subprocess, "check_output", lambda *_args, **_kwargs: csv
    )
    assert cpu_embed.choose_num_gpu() == 99


def test_choose_num_gpu_skips_malformed_csv_rows(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("CPU_EMBED_NUM_GPU", raising=False)
    csv = f"no-comma-row\npython, not-a-number\npython, {cpu_embed.GPU_BUSY_MIB}\n"
    monkeypatch.setattr(
        cpu_embed.subprocess, "check_output", lambda *_args, **_kwargs: csv
    )
    assert cpu_embed.choose_num_gpu() == 0


def test_openai_route_forwards_pinned_ollama_payload(
    monkeypatch: pytest.MonkeyPatch, patched_urlopen
) -> None:
    first = [0.0] * 1024
    second = [0.0] * 1024
    first[0] = 1.0
    second[1] = 1.0
    captured = patched_urlopen(
        cpu_embed, json.dumps({"embeddings": [first, second]}).encode()
    )
    monkeypatch.setenv("CPU_EMBED_MODEL", "qwen3-embed-cpu")
    monkeypatch.setenv("CPU_EMBED_NUM_GPU", "0")
    with TestClient(cpu_embed.build_app()) as client:
        response = client.post(
            "/v1/embeddings", json={"model": "anything", "input": ["a", "b"]}
        )
    assert response.status_code == 200
    body = response.json()
    assert body["object"] == "list"
    assert len(body["data"]) == 2
    assert body["workspace_embedding"]["route_id"] == "local-qwen3"
    assert body["workspace_embedding"]["fingerprint"].startswith("sha256:")
    assert body["workspace_embedding"]["paid"] is False
    payload = captured["request"]
    assert payload["options"]["num_gpu"] == 0
    assert payload["options"]["num_ctx"] == 2048
    assert payload["keep_alive"] == 0


def test_rejects_non_string_input() -> None:
    with TestClient(cpu_embed.build_app()) as client:
        response = client.post("/v1/embeddings", json={"input": [1, 2]})
    assert response.status_code == 400
    assert "error" in response.json()


def test_rejects_malformed_embedding_vectors(
    monkeypatch: pytest.MonkeyPatch, patched_urlopen
) -> None:
    patched_urlopen(cpu_embed, json.dumps({"embeddings": [[0.0, "bad"]]}).encode())
    monkeypatch.setenv("CPU_EMBED_NUM_GPU", "0")
    with pytest.raises(cpu_embed.CpuEmbedError, match="non-numeric"):
        cpu_embed.ollama_embed(["x"], model="test")


def test_unavailable_local_route_falls_back_to_enabled_paid_route(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("WORKSPACE_EMBED_PAID_ENABLED", "1")
    monkeypatch.setenv(
        "WORKSPACE_EMBED_PAID_BASE_URL", "https://embeddings.example/v1/embeddings"
    )
    monkeypatch.setenv("WORKSPACE_EMBED_PAID_API_KEY", "secret")
    monkeypatch.setenv("WORKSPACE_EMBED_PAID_MODEL", "paid-model")
    monkeypatch.setenv("WORKSPACE_EMBED_PAID_MODEL_REVISION", "provider:2026-08-23")
    monkeypatch.setenv("WORKSPACE_EMBED_PAID_DIMENSION", "1536")
    monkeypatch.setenv("WORKSPACE_EMBED_PAID_COST_PER_MILLION_TOKENS", "0.02")
    monkeypatch.setattr(
        cpu_embed,
        "ollama_embed",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(
            cpu_embed.CpuEmbedError("local unavailable")
        ),
    )
    monkeypatch.setattr(cpu_embed, "ensure_ollama_service", lambda: False)
    paid = [0.0] * 1536
    paid[0] = 1.0
    monkeypatch.setattr(
        cpu_embed,
        "openai_compatible_embed",
        lambda *_args, **_kwargs: [paid],
    )

    result = cpu_embed.embed_with_routing(["fallback canary"])

    assert result.attempt.route.route_id == "paid-openai-compatible"
    assert (
        result.attempt.reason
        is embedding_contract.SelectionReason.AVAILABILITY_FALLBACK
    )
    assert result.attempt.route.paid is True


def test_quality_failure_does_not_call_local_route(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    quality_path = tmp_path / "quality.json"
    quality_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "routes": {
                    "local-qwen3": {
                        "status": "FAIL",
                        "route_fingerprint": embedding_contract.route_attempts(
                            embedding_contract.load_routing_config(), env={}
                        )[0].route.fingerprint,
                    }
                },
            }
        ),
        encoding="utf-8",
    )
    for name, value in {
        "WORKSPACE_EMBED_PAID_ENABLED": "1",
        "WORKSPACE_EMBED_PAID_BASE_URL": "https://embeddings.example/v1/embeddings",
        "WORKSPACE_EMBED_PAID_API_KEY": "secret",  # pragma: allowlist secret
        "WORKSPACE_EMBED_PAID_MODEL": "paid-model",
        "WORKSPACE_EMBED_PAID_MODEL_REVISION": "provider:2026-08-23",
        "WORKSPACE_EMBED_PAID_DIMENSION": "1536",
        "WORKSPACE_EMBED_PAID_COST_PER_MILLION_TOKENS": "0.02",
    }.items():
        monkeypatch.setenv(name, value)
    monkeypatch.setattr(
        cpu_embed,
        "ollama_embed",
        lambda *_args, **_kwargs: pytest.fail("quality-failed local route was called"),
    )
    paid = [0.0] * 1536
    paid[0] = 1.0
    monkeypatch.setattr(
        cpu_embed,
        "openai_compatible_embed",
        lambda *_args, **_kwargs: [paid],
    )

    result = cpu_embed.embed_with_routing(
        ["quality canary"],
        quality=embedding_contract.load_quality_receipt(quality_path),
    )

    assert result.attempt.reason is embedding_contract.SelectionReason.QUALITY_FALLBACK


def test_ensure_service_rejects_running_broker_with_different_route(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        cpu_embed,
        "_read_json_url",
        lambda *_args, **_kwargs: {
            "ok": True,
            "route": {"fingerprint": "sha256:" + "0" * 64},
        },
    )

    with pytest.raises(cpu_embed.CpuEmbedError, match="does not match"):
        cpu_embed.ensure_service()


def test_embed_with_routing_skips_route_over_cost_cap(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    route = _route(
        route_id="too-expensive",
        paid=True,
        cost_per_million_tokens=1_000_000_000.0,
        max_request_usd=0.000001,
    )
    _pin_route(monkeypatch, route)

    with pytest.raises(cpu_embed.CpuEmbedError) as excinfo:
        cpu_embed.embed_with_routing(["hello"])

    message = str(excinfo.value)
    assert "too-expensive" in message
    assert "exceeds" in message
    assert "0.000001" in message


def test_embed_with_routing_rechecks_gpu_and_retries_after_ollama_restart(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    route = _route()
    _pin_route(monkeypatch, route)
    gpu_calls: list[int] = []
    monkeypatch.setattr(cpu_embed, "choose_num_gpu", lambda: gpu_calls.append(1) or 0)
    restarts: list[bool] = []
    monkeypatch.setattr(
        cpu_embed, "ensure_ollama_service", lambda: restarts.append(True) or True
    )
    calls: list[int] = []

    def flaky_ollama_embed(*_args: object, **_kwargs: object) -> list[list[float]]:
        calls.append(1)
        if len(calls) == 1:
            raise cpu_embed.CpuEmbedError("connection refused")
        return [[1.0, 0.0]]

    monkeypatch.setattr(cpu_embed, "ollama_embed", flaky_ollama_embed)

    result = cpu_embed.embed_with_routing(["hello"])

    assert result.vectors == [[1.0, 0.0]]
    assert len(calls) == 2
    assert restarts == [True]
    assert len(gpu_calls) == 2


def test_embed_with_routing_rejects_unsupported_protocol(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    route = _route(route_id="carrier-pigeon-route", protocol="carrier-pigeon")
    _pin_route(monkeypatch, route)

    with pytest.raises(cpu_embed.CpuEmbedError) as excinfo:
        cpu_embed.embed_with_routing(["hello"])

    message = str(excinfo.value)
    assert "carrier-pigeon-route" in message
    assert "carrier-pigeon" in message
    assert "unsupported protocol" in message


def test_embed_with_routing_aggregates_failures_from_every_route(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    first = _route(route_id="route-a", protocol="carrier-pigeon")
    second = _route(route_id="route-b", protocol="smoke-signal")
    monkeypatch.setattr(
        cpu_embed,
        "route_attempts",
        lambda *_args, **_kwargs: (_attempt(first), _attempt(second)),
    )

    with pytest.raises(cpu_embed.CpuEmbedError) as excinfo:
        cpu_embed.embed_with_routing(["hello"])

    message = str(excinfo.value)
    assert "route-a" in message
    assert "route-b" in message


def test_embed_with_routing_stops_after_first_failure_when_fingerprint_required(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    first = _route(route_id="route-a", protocol="carrier-pigeon")
    second_called: list[bool] = []
    second = _route(route_id="route-b")
    monkeypatch.setattr(
        cpu_embed,
        "route_attempts",
        lambda *_args, **_kwargs: (_attempt(first), _attempt(second)),
    )
    monkeypatch.setattr(
        cpu_embed,
        "ollama_embed",
        lambda *_args, **_kwargs: second_called.append(True) or [[1.0, 0.0]],
    )

    with pytest.raises(cpu_embed.CpuEmbedError) as excinfo:
        cpu_embed.embed_with_routing(["hello"], required_fingerprint=first.fingerprint)

    assert "route-a" in str(excinfo.value)
    assert "route-b" not in str(excinfo.value)
    assert second_called == []


def test_paid_route_rejects_duplicate_response_indexes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    env = {
        "WORKSPACE_EMBED_PAID_ENABLED": "1",
        "WORKSPACE_EMBED_PAID_BASE_URL": "https://embeddings.example/v1/embeddings",
        "WORKSPACE_EMBED_PAID_API_KEY": "secret",  # pragma: allowlist secret
        "WORKSPACE_EMBED_PAID_MODEL": "paid-model",
        "WORKSPACE_EMBED_PAID_MODEL_REVISION": "provider:2026-08-23",
        "WORKSPACE_EMBED_PAID_DIMENSION": "2",
        "WORKSPACE_EMBED_PAID_COST_PER_MILLION_TOKENS": "0.02",
    }
    for name, value in env.items():
        monkeypatch.setenv(name, value)
    route = embedding_contract.load_routing_config().routes[1].resolve(env)
    assert route is not None
    monkeypatch.setattr(
        cpu_embed,
        "_post_json",
        lambda *_args, **_kwargs: {
            "data": [
                {"index": 0, "embedding": [1.0, 0.0]},
                {"index": 0, "embedding": [0.0, 1.0]},
            ]
        },
    )

    with pytest.raises(cpu_embed.CpuEmbedError, match="invalid indexes"):
        cpu_embed.openai_compatible_embed(["a", "b"], route=route)


def test_post_json_returns_parsed_object_body(
    monkeypatch: pytest.MonkeyPatch, patched_urlopen
) -> None:
    patched_urlopen(cpu_embed, json.dumps({"ok": True, "value": 1}).encode())
    body = cpu_embed._post_json(
        "https://embeddings.example/v1/embeddings", {"input": ["x"]}, timeout_s=1.0
    )
    assert body == {"ok": True, "value": 1}


def test_post_json_rejects_non_http_scheme() -> None:
    # The retained implementation guards the scheme in conductor.http_transport
    # (shared by three callers) rather than inline; the security property asserted
    # here is unchanged — a file:/ endpoint must raise, not be fetched.
    with pytest.raises(cpu_embed.CpuEmbedError, match="must use http or https"):
        cpu_embed._post_json("file:///etc/passwd", {}, timeout_s=1.0)


def test_post_json_wraps_network_error(monkeypatch: pytest.MonkeyPatch) -> None:
    _refuse_urlopen(monkeypatch)
    with pytest.raises(cpu_embed.CpuEmbedError, match="embedding request to"):
        cpu_embed._post_json(
            "https://embeddings.example/v1/embeddings", {}, timeout_s=1.0
        )


def test_post_json_rejects_non_object_response(
    monkeypatch: pytest.MonkeyPatch, patched_urlopen
) -> None:
    patched_urlopen(cpu_embed, json.dumps([1, 2, 3]).encode())
    with pytest.raises(cpu_embed.CpuEmbedError, match="is not an object"):
        cpu_embed._post_json(
            "https://embeddings.example/v1/embeddings", {}, timeout_s=1.0
        )


def test_health_route_reports_primary_route_metadata(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("CPU_EMBED_NUM_GPU", "0")
    with TestClient(cpu_embed.build_app()) as client:
        response = client.get("/health")
    assert response.status_code == 200
    body = response.json()
    assert body["ok"] is True
    assert body["num_gpu"] == 0
    assert body["route"]["route_id"] == "local-qwen3"
    assert body["selection_reason"] == embedding_contract.SelectionReason.PRIMARY.value


def test_health_route_returns_503_when_routing_fails(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _fail_route_attempts(monkeypatch, "no routes configured")
    with TestClient(cpu_embed.build_app()) as client:
        response = client.get("/health")
    assert response.status_code == 503
    assert response.json()["ok"] is False
    assert "no routes configured" in response.json()["error"]


def test_embed_with_routing_rejects_empty_texts() -> None:
    with pytest.raises(cpu_embed.CpuEmbedError, match="must not be empty"):
        cpu_embed.embed_with_routing([])


def test_embed_with_routing_wraps_contract_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _fail_route_attempts(monkeypatch, "pinned index has no route")
    with pytest.raises(cpu_embed.CpuEmbedError, match="pinned index has no route"):
        cpu_embed.embed_with_routing(["hello"])


def test_choose_num_gpu_rejects_non_integer_forced_value(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("CPU_EMBED_NUM_GPU", "not-a-number")
    with pytest.raises(cpu_embed.CpuEmbedError, match="must be 0 or 99"):
        cpu_embed.choose_num_gpu()


def test_pin_options_rejects_invalid_explicit_num_gpu() -> None:
    with pytest.raises(cpu_embed.CpuEmbedError, match="num_gpu must be 0 or 99"):
        cpu_embed.pin_options(num_gpu=7)


def test_ollama_embed_returns_empty_for_no_texts() -> None:
    assert cpu_embed.ollama_embed([], model="test") == []


def test_ollama_embed_wraps_network_error(monkeypatch: pytest.MonkeyPatch) -> None:
    _refuse_urlopen(monkeypatch)
    with pytest.raises(cpu_embed.CpuEmbedError, match="ollama embed failed"):
        cpu_embed.ollama_embed(["x"], model="test")


def test_ollama_embed_rejects_wrong_vector_count(
    monkeypatch: pytest.MonkeyPatch, patched_urlopen
) -> None:
    patched_urlopen(cpu_embed, json.dumps({"embeddings": [[1.0, 0.0]]}).encode())
    with pytest.raises(cpu_embed.CpuEmbedError, match="wrong vector count"):
        cpu_embed.ollama_embed(["a", "b"], model="test")


def test_ollama_embed_rejects_empty_vector(
    monkeypatch: pytest.MonkeyPatch, patched_urlopen
) -> None:
    patched_urlopen(cpu_embed, json.dumps({"embeddings": [[]]}).encode())
    with pytest.raises(cpu_embed.CpuEmbedError, match="not a non-empty array"):
        cpu_embed.ollama_embed(["a"], model="test")


def test_l2_normalize_rejects_zero_vector() -> None:
    with pytest.raises(cpu_embed.CpuEmbedError, match="zero or non-finite"):
        cpu_embed._l2_normalize([0.0, 0.0], route_id="test-route")


def _paid_route(**overrides: object) -> embedding_contract.ResolvedRoute:
    return _route(
        route_id="paid-route",
        backend_kind="paid",
        protocol="openai-embeddings",
        paid=True,
        endpoint="https://embeddings.example/v1/embeddings",
        api_key_env="TEST_PAID_API_KEY",  # pragma: allowlist secret
        **overrides,
    )


def test_openai_compatible_embed_requires_credential_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("TEST_PAID_API_KEY", raising=False)
    with pytest.raises(cpu_embed.CpuEmbedError, match="missing credential env"):
        cpu_embed.openai_compatible_embed(["a"], route=_paid_route())


def test_openai_compatible_embed_rejects_non_list_data(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("TEST_PAID_API_KEY", "key")
    monkeypatch.setattr(cpu_embed, "_post_json", lambda *_a, **_k: {"data": "nope"})
    with pytest.raises(cpu_embed.CpuEmbedError, match="lacks data"):
        cpu_embed.openai_compatible_embed(["a"], route=_paid_route())


def test_openai_compatible_embed_rejects_row_missing_embedding(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("TEST_PAID_API_KEY", "key")
    monkeypatch.setattr(
        cpu_embed, "_post_json", lambda *_a, **_k: {"data": [{"index": 0}]}
    )
    with pytest.raises(cpu_embed.CpuEmbedError, match="lacks an embedding"):
        cpu_embed.openai_compatible_embed(["a"], route=_paid_route())


def test_openai_compatible_embed_rejects_invalid_index(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("TEST_PAID_API_KEY", "key")
    monkeypatch.setattr(
        cpu_embed,
        "_post_json",
        lambda *_a, **_k: {"data": [{"index": "zero", "embedding": [1.0]}]},
    )
    with pytest.raises(cpu_embed.CpuEmbedError, match="has invalid index"):
        cpu_embed.openai_compatible_embed(["a"], route=_paid_route())


def test_openai_compatible_embed_rejects_non_numeric_value(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("TEST_PAID_API_KEY", "key")
    monkeypatch.setattr(
        cpu_embed,
        "_post_json",
        lambda *_a, **_k: {"data": [{"index": 0, "embedding": [1.0, "bad"]}]},
    )
    with pytest.raises(cpu_embed.CpuEmbedError, match="is not numeric"):
        cpu_embed.openai_compatible_embed(["a"], route=_paid_route())


def test_openai_compatible_embed_returns_vectors_ordered_by_index(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("TEST_PAID_API_KEY", "key")
    monkeypatch.setattr(
        cpu_embed,
        "_post_json",
        lambda *_a, **_k: {
            "data": [
                {"index": 1, "embedding": [0.0, 1.0]},
                {"index": 0, "embedding": [1.0, 0.0]},
            ]
        },
    )
    vectors = cpu_embed.openai_compatible_embed(["a", "b"], route=_paid_route())
    assert vectors == [[1.0, 0.0], [0.0, 1.0]]


def test_startup_lock_serializes_and_releases_a_real_file_lock(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    lock_path = tmp_path / "test.lock"
    monkeypatch.setattr(cpu_embed, "_runtime_lock_path", lambda _name: lock_path)

    entered = []
    with cpu_embed._startup_lock("probe"):
        entered.append(True)
        assert lock_path.is_file()
    assert entered == [True]

    # The lock must be released: a second acquisition must not block.
    with cpu_embed._startup_lock("probe"):
        entered.append(True)
    assert entered == [True, True]


def test_read_json_url_returns_parsed_object(
    monkeypatch: pytest.MonkeyPatch, patched_urlopen
) -> None:
    patched_urlopen(cpu_embed, json.dumps({"ok": True}).encode())
    assert cpu_embed._read_json_url(
        "https://embeddings.example/health", timeout=1.0
    ) == {"ok": True}


def test_read_json_url_rejects_non_object_response(
    monkeypatch: pytest.MonkeyPatch, patched_urlopen
) -> None:
    patched_urlopen(cpu_embed, json.dumps([1, 2]).encode())
    with pytest.raises(cpu_embed.CpuEmbedError, match="is not an object"):
        cpu_embed._read_json_url("https://embeddings.example/health", timeout=1.0)


def test_validate_route_vectors_rejects_count_mismatch() -> None:
    route = _route(dimension=2)
    with pytest.raises(cpu_embed.CpuEmbedError, match="returned 1 vectors; expected 2"):
        cpu_embed._validate_route_vectors([[0.0, 0.0]], route, 2)


def test_validate_route_vectors_rejects_dimension_mismatch() -> None:
    route = _route(dimension=2)
    with pytest.raises(cpu_embed.CpuEmbedError, match=r"vector 1 dimension 3 != 2"):
        cpu_embed._validate_route_vectors([[0.0, 0.0], [0.0, 0.0, 0.0]], route, 2)


def test_validate_route_vectors_rejects_non_finite_values() -> None:
    route = _route(dimension=2)
    with pytest.raises(
        cpu_embed.CpuEmbedError, match="vector 0 contains non-finite values"
    ):
        cpu_embed._validate_route_vectors([[0.0, float("nan")]], route, 1)


def test_validate_route_vectors_normalizes_only_for_l2_client_routes() -> None:
    plain_route = _route(dimension=2, normalization="none")
    assert cpu_embed._validate_route_vectors([[3.0, 4.0]], plain_route, 1) == [
        [3.0, 4.0]
    ]

    l2_route = _route(dimension=2, normalization="l2-client")
    [normalized] = cpu_embed._validate_route_vectors([[3.0, 4.0]], l2_route, 1)
    assert normalized == pytest.approx([0.6, 0.8])


def test_resolve_model_returns_primary_route_model(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    route = _route(model="primary-model")
    _pin_route(monkeypatch, route)
    assert cpu_embed.resolve_model() == "primary-model"


def test_resolve_model_falls_back_to_env_or_default_on_contract_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _fail_route_attempts(monkeypatch, "no routes configured")

    monkeypatch.delenv("CPU_EMBED_MODEL", raising=False)
    assert cpu_embed.resolve_model() == cpu_embed.DEFAULT_MODEL

    monkeypatch.setenv("CPU_EMBED_MODEL", "legacy-model")
    assert cpu_embed.resolve_model() == "legacy-model"


def test_models_route_lists_every_attempt_as_openai_style_model(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    with TestClient(cpu_embed.build_app()) as client:
        response = client.get("/v1/models")
    assert response.status_code == 200
    body = response.json()
    assert body["object"] == "list"
    ids = {entry["id"] for entry in body["data"]}
    assert "qwen3-embed-cpu" in ids
    first = body["data"][0]
    assert set(first) == {"id", "object", "owned_by", "route_id", "paid"}
    assert first["object"] == "model"


def test_models_route_propagates_contract_error_uncaught(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _fail_route_attempts(monkeypatch, "no routes configured")
    with TestClient(cpu_embed.build_app(), raise_server_exceptions=True) as client:
        with pytest.raises(embedding_contract.EmbeddingContractError):
            client.get("/v1/models")
