#!/usr/bin/env python3
"""Provider-neutral embedding routes and vector-space provenance.

The contract deliberately separates a provider from its wire protocol.  A local
Ollama route and a paid OpenAI-compatible route can implement the same embedding
capability, while their fingerprints keep incompatible vector spaces apart.
"""

from __future__ import annotations

import hashlib
import json
import os
import tomllib
import urllib.parse
from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path
from typing import Any, Final, Mapping

from conductor.project_paths import host_root

ROOT: Final[Path] = host_root()
ROUTES_PATH: Final[Path] = ROOT / "conductor" / "embedding_routes.toml"
CONTRACT_VERSION: Final[int] = 1
QUALITY_SCHEMA_VERSION: Final[int] = 1
QUALITY_RECEIPT_ENV: Final[str] = "WORKSPACE_EMBED_QUALITY_RECEIPT"


class EmbeddingContractError(RuntimeError):
    """A route, quality receipt, or vector-space contract is invalid."""


class QualityStatus(StrEnum):
    PASS = "PASS"
    FAIL = "FAIL"
    UNKNOWN = "UNKNOWN"


class SelectionReason(StrEnum):
    PRIMARY = "primary"
    QUALITY_FALLBACK = "quality-fallback"
    AVAILABILITY_FALLBACK = "availability-fallback"
    PINNED_INDEX = "pinned-index"


@dataclass(frozen=True, slots=True)
class RouteSpec:
    """One configured embedding implementation without resolved secrets."""

    route_id: str
    backend_kind: str
    protocol: str
    priority: int
    normalization: str
    tokenizer_policy: str
    truncation_policy: str
    max_input_chars: int
    device_policy: str
    paid: bool
    endpoint: str = ""
    endpoint_env: str = ""
    api_key_env: str = ""
    enabled_env: str = ""
    model: str = ""
    model_env: str = ""
    model_revision: str = ""
    model_revision_env: str = ""
    dimension: int = 0
    dimension_env: str = ""
    cost_per_million_tokens_env: str = ""
    max_request_usd: float = 0.0

    def resolve(self, env: Mapping[str, str] | None = None) -> ResolvedRoute | None:
        values = os.environ if env is None else env
        if self.enabled_env and values.get(self.enabled_env, "").strip() != "1":
            return None
        if (
            self.paid
            and self.api_key_env
            and not values.get(self.api_key_env, "").strip()
        ):
            return None
        endpoint = _env_or_value(values, self.endpoint_env, self.endpoint)
        model = _env_or_value(values, self.model_env, self.model)
        revision = _env_or_value(values, self.model_revision_env, self.model_revision)
        dimension_text = _env_or_value(
            values,
            self.dimension_env,
            str(self.dimension) if self.dimension else "",
        )
        missing = [
            name
            for name, value in (
                ("endpoint", endpoint),
                ("model", model),
                ("model_revision", revision),
                ("dimension", dimension_text),
            )
            if not value
        ]
        if missing:
            if self.paid:
                return None
            raise EmbeddingContractError(
                f"route {self.route_id!r} is missing required fields: {missing}"
            )
        try:
            dimension = int(dimension_text)
        except ValueError as exc:
            raise EmbeddingContractError(
                f"route {self.route_id!r} dimension must be an integer, "
                f"got {dimension_text!r}"
            ) from exc
        if dimension < 1:
            raise EmbeddingContractError(
                f"route {self.route_id!r} dimension must be positive, got {dimension}"
            )
        _validate_endpoint(self.route_id, endpoint, paid=self.paid)
        cost = 0.0
        if self.cost_per_million_tokens_env:
            raw_cost = values.get(self.cost_per_million_tokens_env, "").strip()
            if self.paid and not raw_cost:
                return None
            try:
                cost = float(raw_cost or "0")
            except ValueError as exc:
                raise EmbeddingContractError(
                    f"route {self.route_id!r} cost must be numeric, got {raw_cost!r}"
                ) from exc
            if cost < 0:
                raise EmbeddingContractError(
                    f"route {self.route_id!r} cost must be non-negative"
                )
        return ResolvedRoute(
            route_id=self.route_id,
            backend_kind=self.backend_kind,
            protocol=self.protocol,
            priority=self.priority,
            normalization=self.normalization,
            tokenizer_policy=self.tokenizer_policy,
            truncation_policy=self.truncation_policy,
            max_input_chars=self.max_input_chars,
            device_policy=self.device_policy,
            paid=self.paid,
            endpoint=endpoint,
            api_key_env=self.api_key_env,
            model=model,
            model_revision=revision,
            dimension=dimension,
            cost_per_million_tokens=cost,
            max_request_usd=self.max_request_usd,
        )


@dataclass(frozen=True, slots=True)
class ResolvedRoute:
    """A usable route whose public fields define one vector space."""

    route_id: str
    backend_kind: str
    protocol: str
    priority: int
    normalization: str
    tokenizer_policy: str
    truncation_policy: str
    max_input_chars: int
    device_policy: str
    paid: bool
    endpoint: str
    api_key_env: str
    model: str
    model_revision: str
    dimension: int
    cost_per_million_tokens: float
    max_request_usd: float

    @property
    def fingerprint(self) -> str:
        payload = self.vector_space_payload()
        canonical = json.dumps(payload, sort_keys=True, separators=(",", ":"))
        return "sha256:" + hashlib.sha256(canonical.encode()).hexdigest()

    def vector_space_payload(self) -> dict[str, Any]:
        return {
            "contract_version": CONTRACT_VERSION,
            "route_id": self.route_id,
            "backend_kind": self.backend_kind,
            "protocol": self.protocol,
            "endpoint": self.endpoint,
            "model": self.model,
            "model_revision": self.model_revision,
            "dimension": self.dimension,
            "normalization": self.normalization,
            "tokenizer_policy": self.tokenizer_policy,
            "truncation_policy": self.truncation_policy,
            "max_input_chars": self.max_input_chars,
        }

    def public_metadata(self) -> dict[str, Any]:
        return {
            **self.vector_space_payload(),
            "fingerprint": self.fingerprint,
            "device_policy": self.device_policy,
            "paid": self.paid,
            "cost_per_million_tokens": self.cost_per_million_tokens,
            "max_request_usd": self.max_request_usd,
        }

    def estimate_request_cost(self, texts: list[str]) -> float:
        estimated_tokens = max(1, sum(len(text) for text in texts) // 4)
        return estimated_tokens * self.cost_per_million_tokens / 1_000_000.0


@dataclass(frozen=True, slots=True)
class QualityResult:
    status: QualityStatus
    route_fingerprint: str
    score: float | None = None
    threshold: float | None = None
    fixture_sha256: str = ""


@dataclass(frozen=True, slots=True)
class QualityReceipt:
    path: Path | None
    sha256: str
    routes: Mapping[str, QualityResult]

    def result_for(self, route_id: str) -> QualityResult | None:
        return self.routes.get(route_id)


@dataclass(frozen=True, slots=True)
class RoutingConfig:
    primary_route: str
    fallback_routes: tuple[str, ...]
    routes: tuple[RouteSpec, ...]

    def by_id(self) -> dict[str, RouteSpec]:
        return {route.route_id: route for route in self.routes}


@dataclass(frozen=True, slots=True)
class RouteAttempt:
    route: ResolvedRoute
    reason: SelectionReason


def _env_or_value(values: Mapping[str, str], env_name: str, value: str) -> str:
    return values.get(env_name, "").strip() if env_name else value.strip()


def _validate_endpoint(route_id: str, endpoint: str, *, paid: bool) -> None:
    parsed = urllib.parse.urlparse(endpoint)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise EmbeddingContractError(
            f"route {route_id!r} endpoint must be an absolute HTTP(S) URL"
        )
    loopback = parsed.hostname in {"127.0.0.1", "localhost", "::1"}
    if not paid and not loopback:
        raise EmbeddingContractError(
            f"local route {route_id!r} must use loopback, got {endpoint!r}"
        )
    if paid and not loopback and parsed.scheme != "https":
        raise EmbeddingContractError(
            f"paid route {route_id!r} must use HTTPS, got {endpoint!r}"
        )


def load_routing_config(path: Path = ROUTES_PATH) -> RoutingConfig:
    try:
        payload = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise EmbeddingContractError(
            f"embedding routes unreadable: {path}: {exc}"
        ) from exc
    if payload.get("schema_version") != CONTRACT_VERSION:
        raise EmbeddingContractError(
            f"unsupported embedding route schema {payload.get('schema_version')!r}"
        )
    policy = payload.get("policy")
    raw_routes = payload.get("route")
    if not isinstance(policy, dict) or not isinstance(raw_routes, list):
        raise EmbeddingContractError("embedding routes require [policy] and [[route]]")
    try:
        routes = tuple(RouteSpec(**raw) for raw in raw_routes)
        config = RoutingConfig(
            primary_route=str(policy["primary_route"]),
            fallback_routes=tuple(str(item) for item in policy["fallback_routes"]),
            routes=routes,
        )
    except (KeyError, TypeError, ValueError) as exc:
        raise EmbeddingContractError(
            f"embedding route config is invalid: {exc}"
        ) from exc
    route_ids = [route.route_id for route in routes]
    if len(route_ids) != len(set(route_ids)):
        raise EmbeddingContractError("embedding route IDs must be unique")
    required = {config.primary_route, *config.fallback_routes}
    missing = sorted(required - set(route_ids))
    if missing:
        raise EmbeddingContractError(
            f"embedding policy references missing routes: {missing}"
        )
    return config


def load_quality_receipt(
    path: Path | None = None,
    *,
    env: Mapping[str, str] | None = None,
) -> QualityReceipt:
    values = os.environ if env is None else env
    selected = path
    if selected is None:
        raw_path = values.get(QUALITY_RECEIPT_ENV, "").strip()
        selected = Path(raw_path) if raw_path else None
    if selected is None:
        return QualityReceipt(path=None, sha256="", routes={})
    try:
        raw = selected.read_bytes()
        payload = json.loads(raw)
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise EmbeddingContractError(
            f"embedding quality receipt unreadable: {selected}: {exc}"
        ) from exc
    if (
        not isinstance(payload, dict)
        or payload.get("schema_version") != QUALITY_SCHEMA_VERSION
    ):
        raise EmbeddingContractError("embedding quality receipt schema is invalid")
    raw_routes = payload.get("routes")
    if not isinstance(raw_routes, dict):
        raise EmbeddingContractError("embedding quality receipt routes are missing")
    routes: dict[str, QualityResult] = {}
    for route_id, raw_result in raw_routes.items():
        if not isinstance(route_id, str) or not isinstance(raw_result, dict):
            raise EmbeddingContractError("embedding quality route result is invalid")
        try:
            status = QualityStatus(str(raw_result["status"]))
            route_fingerprint = str(raw_result["route_fingerprint"])
            score = _optional_float(raw_result.get("score"))
            threshold = _optional_float(raw_result.get("threshold"))
        except (KeyError, TypeError, ValueError) as exc:
            raise EmbeddingContractError(
                f"quality result for {route_id!r} is invalid: {exc}"
            ) from exc
        if not route_fingerprint.startswith("sha256:"):
            raise EmbeddingContractError(
                f"quality result for {route_id!r} has an invalid route fingerprint"
            )
        if status is QualityStatus.PASS and score is not None and threshold is not None:
            if score < threshold:
                raise EmbeddingContractError(
                    f"quality result for {route_id!r} claims PASS below threshold"
                )
        if status is QualityStatus.FAIL and score is not None and threshold is not None:
            if score >= threshold:
                raise EmbeddingContractError(
                    f"quality result for {route_id!r} claims FAIL at/above threshold"
                )
        routes[route_id] = QualityResult(
            status=status,
            route_fingerprint=route_fingerprint,
            score=score,
            threshold=threshold,
            fixture_sha256=str(raw_result.get("fixture_sha256", "")),
        )
    return QualityReceipt(
        path=selected,
        sha256=hashlib.sha256(raw).hexdigest(),
        routes=routes,
    )


def _optional_float(value: object) -> float | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise TypeError(f"expected numeric value, got {value!r}")
    return float(value)


def route_attempts(
    config: RoutingConfig,
    *,
    quality: QualityReceipt | None = None,
    required_fingerprint: str = "",
    env: Mapping[str, str] | None = None,
) -> tuple[RouteAttempt, ...]:
    """Return the allowed route order without probing or hiding failures.

    A pinned index may use only its matching vector space.  An unpinned build or
    compatibility request prefers local, but skips it when a measured quality
    receipt marks it FAIL and an approved paid route is configured.
    """

    quality = quality or QualityReceipt(path=None, sha256="", routes={})
    resolved = {
        route.route_id: candidate
        for route in config.routes
        if (candidate := route.resolve(env)) is not None
    }
    if required_fingerprint:
        matches = [
            route
            for route in resolved.values()
            if route.fingerprint == required_fingerprint
        ]
        if len(matches) != 1:
            raise EmbeddingContractError(
                "no configured embedding route matches required fingerprint "
                f"{required_fingerprint!r}"
            )
        return (RouteAttempt(matches[0], SelectionReason.PINNED_INDEX),)

    def quality_status(route: ResolvedRoute) -> QualityStatus:
        result = quality.result_for(route.route_id)
        if result is None:
            return QualityStatus.UNKNOWN
        if result.route_fingerprint != route.fingerprint:
            raise EmbeddingContractError(
                f"quality result for {route.route_id!r} is bound to "
                f"{result.route_fingerprint!r}, not {route.fingerprint!r}"
            )
        return result.status

    primary = resolved.get(config.primary_route)
    primary_quality = (
        QualityStatus.UNKNOWN if primary is None else quality_status(primary)
    )
    attempts: list[RouteAttempt] = []
    if primary is not None and primary_quality is not QualityStatus.FAIL:
        attempts.append(RouteAttempt(primary, SelectionReason.PRIMARY))
        fallback_reason = SelectionReason.AVAILABILITY_FALLBACK
    else:
        fallback_reason = (
            SelectionReason.QUALITY_FALLBACK
            if primary_quality is QualityStatus.FAIL
            else SelectionReason.AVAILABILITY_FALLBACK
        )
    for route_id in config.fallback_routes:
        route = resolved.get(route_id)
        if route is None or quality_status(route) is QualityStatus.FAIL:
            continue
        attempts.append(RouteAttempt(route, fallback_reason))
    if not attempts:
        raise EmbeddingContractError(
            "no approved embedding route is available for the current quality policy"
        )
    return tuple(attempts)
