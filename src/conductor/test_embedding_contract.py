from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor import embedding_contract as contract


def _paid_env() -> dict[str, str]:
    return {
        "WORKSPACE_EMBED_PAID_ENABLED": "1",
        "WORKSPACE_EMBED_PAID_BASE_URL": "https://embeddings.example/v1/embeddings",
        "WORKSPACE_EMBED_PAID_API_KEY": "secret",  # pragma: allowlist secret
        "WORKSPACE_EMBED_PAID_MODEL": "paid-model",
        "WORKSPACE_EMBED_PAID_MODEL_REVISION": "provider:2026-08-23",
        "WORKSPACE_EMBED_PAID_DIMENSION": "1536",
        "WORKSPACE_EMBED_PAID_COST_PER_MILLION_TOKENS": "0.02",
    }


def _local_fingerprint() -> str:
    return contract.route_attempts(contract.load_routing_config(), env={})[
        0
    ].route.fingerprint


def _route_spec(**overrides: object) -> contract.RouteSpec:
    fields: dict[str, object] = dict(
        route_id="r",
        backend_kind="local",
        protocol="ollama-embed",
        priority=1,
        normalization="l2-client",
        tokenizer_policy="backend-default",
        truncation_policy="unicode-prefix",
        max_input_chars=100,
        device_policy="cpu",
        paid=False,
        endpoint="http://127.0.0.1:9/embed",
        model="m",
        model_revision="m:1",
        dimension=8,
    )
    fields.update(overrides)
    return contract.RouteSpec(**fields)


_ROUTE_TOML_FIELDS = """\
backend_kind = "local"
protocol = "ollama-embed"
priority = 1
normalization = "l2-client"
tokenizer_policy = "backend-default"
truncation_policy = "unicode-prefix"
max_input_chars = 100
device_policy = "cpu"
paid = false
"""


def test_resolve_enforces_required_fields_dimension_and_cost() -> None:
    # not paid: missing fields raise; paid: the same gap is a soft skip
    with pytest.raises(
        contract.EmbeddingContractError, match="missing required fields"
    ):
        _route_spec(endpoint="", model="").resolve({})
    assert _route_spec(paid=True, endpoint="", model="").resolve({}) is None

    # paid + configured api_key_env with no value resolved: soft skip
    key_route = _route_spec(paid=True, api_key_env="X_KEY")  # pragma: allowlist secret
    assert key_route.resolve({}) is None

    dim_route = _route_spec(dimension=0, dimension_env="DIM")
    with pytest.raises(contract.EmbeddingContractError, match="must be an integer"):
        dim_route.resolve({"DIM": "abc"})
    with pytest.raises(contract.EmbeddingContractError, match="must be positive"):
        dim_route.resolve({"DIM": "0"})

    cost_route = _route_spec(
        paid=True,
        endpoint="https://e.example/v1",
        cost_per_million_tokens_env="COST",
    )
    assert cost_route.resolve({}) is None
    with pytest.raises(contract.EmbeddingContractError, match="cost must be numeric"):
        cost_route.resolve({"COST": "free"})
    with pytest.raises(contract.EmbeddingContractError, match="non-negative"):
        cost_route.resolve({"COST": "-1"})


@pytest.mark.parametrize(
    ("dimension_value", "should_raise"), [("0", True), ("1", False)]
)
def test_resolve_dimension_boundary(dimension_value: str, should_raise: bool) -> None:
    dim_route = _route_spec(dimension=0, dimension_env="DIM")
    if should_raise:
        with pytest.raises(contract.EmbeddingContractError, match="must be positive"):
            dim_route.resolve({"DIM": dimension_value})
    else:
        assert dim_route.resolve({"DIM": dimension_value}) is not None


def test_by_id_maps_route_id_to_spec() -> None:
    config = contract.load_routing_config()
    mapping = config.by_id()
    assert mapping["local-qwen3"].route_id == "local-qwen3"


def test_validate_endpoint_rejects_bad_scheme_and_non_https_paid() -> None:
    with pytest.raises(contract.EmbeddingContractError, match="absolute HTTP"):
        contract._validate_endpoint("r", "not-a-url", paid=False)
    with pytest.raises(contract.EmbeddingContractError, match="must use HTTPS"):
        contract._validate_endpoint("r", "http://e.example/v1", paid=True)


def test_load_routing_config_rejects_malformed_variants(tmp_path: Path) -> None:
    with pytest.raises(contract.EmbeddingContractError, match="unreadable"):
        contract.load_routing_config(tmp_path / "missing.toml")

    unsupported = tmp_path / "unsupported.toml"
    unsupported.write_text("schema_version = 99\n", encoding="utf-8")
    with pytest.raises(contract.EmbeddingContractError, match="unsupported"):
        contract.load_routing_config(unsupported)

    no_policy = tmp_path / "no_policy.toml"
    no_policy.write_text(
        f"schema_version = {contract.CONTRACT_VERSION}\n", encoding="utf-8"
    )
    with pytest.raises(contract.EmbeddingContractError, match="require"):
        contract.load_routing_config(no_policy)

    bad_route = tmp_path / "bad_route.toml"
    bad_route.write_text(
        f"schema_version = {contract.CONTRACT_VERSION}\n"
        '[policy]\nprimary_route = "r"\nfallback_routes = []\n'
        '[[route]]\nroute_id = "r"\n',
        encoding="utf-8",
    )
    with pytest.raises(contract.EmbeddingContractError, match="invalid"):
        contract.load_routing_config(bad_route)

    dup_ids = tmp_path / "dup_ids.toml"
    route_block = '[[route]]\nroute_id = "dup"\n' + _ROUTE_TOML_FIELDS
    dup_ids.write_text(
        f"schema_version = {contract.CONTRACT_VERSION}\n"
        '[policy]\nprimary_route = "dup"\nfallback_routes = []\n'
        + route_block
        + route_block,
        encoding="utf-8",
    )
    with pytest.raises(contract.EmbeddingContractError, match="unique"):
        contract.load_routing_config(dup_ids)

    missing_ref = tmp_path / "missing_ref.toml"
    missing_ref.write_text(
        f"schema_version = {contract.CONTRACT_VERSION}\n"
        '[policy]\nprimary_route = "ghost"\nfallback_routes = []\n'
        '[[route]]\nroute_id = "real"\n' + _ROUTE_TOML_FIELDS,
        encoding="utf-8",
    )
    with pytest.raises(contract.EmbeddingContractError, match="missing routes"):
        contract.load_routing_config(missing_ref)


def test_load_quality_receipt_rejects_malformed_variants(tmp_path: Path) -> None:
    bad_json = tmp_path / "bad.json"
    bad_json.write_text("not json", encoding="utf-8")
    with pytest.raises(contract.EmbeddingContractError, match="unreadable"):
        contract.load_quality_receipt(bad_json)

    bad_schema = tmp_path / "bad_schema.json"
    bad_schema.write_text(json.dumps({"schema_version": 999}), encoding="utf-8")
    with pytest.raises(contract.EmbeddingContractError, match="schema is invalid"):
        contract.load_quality_receipt(bad_schema)

    no_routes = tmp_path / "no_routes.json"
    no_routes.write_text(
        json.dumps({"schema_version": contract.QUALITY_SCHEMA_VERSION}),
        encoding="utf-8",
    )
    with pytest.raises(contract.EmbeddingContractError, match="routes are missing"):
        contract.load_quality_receipt(no_routes)

    bad_result_type = tmp_path / "bad_result_type.json"
    bad_result_type.write_text(
        json.dumps(
            {
                "schema_version": contract.QUALITY_SCHEMA_VERSION,
                "routes": {"r": "not-a-dict"},
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(
        contract.EmbeddingContractError, match="route result is invalid"
    ):
        contract.load_quality_receipt(bad_result_type)

    bad_status = tmp_path / "bad_status.json"
    bad_status.write_text(
        json.dumps(
            {
                "schema_version": contract.QUALITY_SCHEMA_VERSION,
                "routes": {
                    "r": {
                        "status": "NOT_A_STATUS",
                        "route_fingerprint": "sha256:" + "0" * 64,
                    }
                },
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(contract.EmbeddingContractError, match="is invalid"):
        contract.load_quality_receipt(bad_status)

    bad_fingerprint = tmp_path / "bad_fingerprint.json"
    bad_fingerprint.write_text(
        json.dumps(
            {
                "schema_version": contract.QUALITY_SCHEMA_VERSION,
                "routes": {"r": {"status": "FAIL", "route_fingerprint": "not-sha256"}},
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(
        contract.EmbeddingContractError, match="invalid route fingerprint"
    ):
        contract.load_quality_receipt(bad_fingerprint)

    fail_above_threshold = tmp_path / "fail_above_threshold.json"
    fail_above_threshold.write_text(
        json.dumps(
            {
                "schema_version": contract.QUALITY_SCHEMA_VERSION,
                "routes": {
                    "r": {
                        "status": "FAIL",
                        "route_fingerprint": "sha256:" + "0" * 64,
                        "score": 0.9,
                        "threshold": 0.8,
                    }
                },
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(
        contract.EmbeddingContractError, match="claims FAIL at/above threshold"
    ):
        contract.load_quality_receipt(fail_above_threshold)


def test_optional_float_rejects_non_numeric_and_bool() -> None:
    assert contract._optional_float(None) is None
    assert contract._optional_float(3) == 3.0
    with pytest.raises(TypeError, match="expected numeric"):
        contract._optional_float("nope")
    with pytest.raises(TypeError, match="expected numeric"):
        contract._optional_float(True)


def test_route_attempts_raises_when_fingerprint_matches_no_route() -> None:
    with pytest.raises(
        contract.EmbeddingContractError, match="no configured embedding route matches"
    ):
        contract.route_attempts(
            contract.load_routing_config(),
            required_fingerprint="sha256:" + "f" * 64,
            env={},
        )


def test_default_route_is_local_and_provider_neutral() -> None:
    config = contract.load_routing_config()
    attempts = contract.route_attempts(config, env={})

    assert [attempt.route.route_id for attempt in attempts] == ["local-qwen3"]
    route = attempts[0].route
    assert route.protocol == "ollama-embed"
    assert route.paid is False
    assert route.fingerprint.startswith("sha256:")
    assert "api_key" not in route.public_metadata()


def test_quality_failure_selects_explicit_paid_fallback(tmp_path: Path) -> None:
    receipt = tmp_path / "quality.json"
    receipt.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "routes": {
                    "local-qwen3": {
                        "status": "FAIL",
                        "route_fingerprint": _local_fingerprint(),
                        "score": 0.61,
                        "threshold": 0.75,
                        "fixture_sha256": "a" * 64,
                    }
                },
            }
        ),
        encoding="utf-8",
    )
    config = contract.load_routing_config()
    quality = contract.load_quality_receipt(receipt)

    attempts = contract.route_attempts(config, quality=quality, env=_paid_env())

    assert len(attempts) == 1
    assert attempts[0].route.route_id == "paid-openai-compatible"
    assert attempts[0].reason is contract.SelectionReason.QUALITY_FALLBACK
    assert attempts[0].route.paid is True


def test_paid_route_is_disabled_without_explicit_configuration(
    tmp_path: Path,
) -> None:
    receipt = tmp_path / "quality.json"
    receipt.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "routes": {
                    "local-qwen3": {
                        "status": "FAIL",
                        "route_fingerprint": _local_fingerprint(),
                    }
                },
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(contract.EmbeddingContractError, match="no approved"):
        contract.route_attempts(
            contract.load_routing_config(),
            quality=contract.load_quality_receipt(receipt),
            env={},
        )


def test_pinned_index_never_switches_vector_spaces() -> None:
    config = contract.load_routing_config()
    local = contract.route_attempts(config, env={})[0].route

    attempts = contract.route_attempts(
        config,
        required_fingerprint=local.fingerprint,
        env=_paid_env(),
    )

    assert len(attempts) == 1
    assert attempts[0].route.route_id == "local-qwen3"
    assert attempts[0].reason is contract.SelectionReason.PINNED_INDEX


def test_rejects_non_loopback_local_endpoint(tmp_path: Path) -> None:
    path = tmp_path / "routes.toml"
    path.write_text(
        """
schema_version = 1
[policy]
primary_route = "bad"
fallback_routes = []
[[route]]
route_id = "bad"
backend_kind = "local"
protocol = "ollama-embed"
priority = 1
normalization = "l2-client"
tokenizer_policy = "backend-default"
truncation_policy = "unicode-prefix"
max_input_chars = 100
device_policy = "cpu"
paid = false
endpoint = "http://example.com/embed"
model = "x"
model_revision = "x:1"
dimension = 2
""".strip(),
        encoding="utf-8",
    )
    config = contract.load_routing_config(path)
    with pytest.raises(contract.EmbeddingContractError, match="loopback"):
        contract.route_attempts(config, env={})


def test_quality_receipt_cannot_claim_pass_below_threshold(tmp_path: Path) -> None:
    path = tmp_path / "quality.json"
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "routes": {
                    "local-qwen3": {
                        "status": "PASS",
                        "route_fingerprint": _local_fingerprint(),
                        "score": 0.5,
                        "threshold": 0.8,
                    }
                },
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(contract.EmbeddingContractError, match="below threshold"):
        contract.load_quality_receipt(path)


def test_quality_receipt_is_bound_to_exact_route_fingerprint(
    tmp_path: Path,
) -> None:
    path = tmp_path / "quality.json"
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "routes": {
                    "local-qwen3": {
                        "status": "FAIL",
                        "route_fingerprint": "sha256:" + "0" * 64,
                    }
                },
            }
        ),
        encoding="utf-8",
    )

    with pytest.raises(contract.EmbeddingContractError, match="is bound to"):
        contract.route_attempts(
            contract.load_routing_config(),
            quality=contract.load_quality_receipt(path),
            env=_paid_env(),
        )
