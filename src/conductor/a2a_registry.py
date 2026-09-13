"""Private registry and liveness primitives for the local A2A transport."""

from __future__ import annotations

import contextlib
import dataclasses
import datetime as dt
import fcntl
import hashlib
import json
import os
import re
import secrets
import stat
from collections.abc import Iterator
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any, Final

import httpx
from a2a.utils.constants import AGENT_CARD_WELL_KNOWN_PATH


from conductor.project_paths import host_root
ROOT: Final = host_root()
DEFAULT_STATE_DIR: Final = ROOT / ".agents" / "a2a"
BIND_HOST: Final = "127.0.0.1"
SCHEMA_VERSION: Final = 1
LIVENESS_SCHEMA_VERSION: Final = 1
PROBE_TIMEOUT_S: Final = 1.0
DEFAULT_REAP_FAILURES: Final = 3
IDENTITY_RE: Final = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
# Port assignments are a catalog, not a roster. They are used only when a
# named identity is explicitly initialized or lazily started.
KNOWN_AGENTS: Final[dict[str, int]] = {
    "codex-phase22": 7310,
    "glm-5.3": 7311,
    "fable-nmf6": 7312,
    "claude-opus-5": 7313,
    "antigravity": 7314,
    "fable-helm": 7315,
    "grok": 7316,
}


class A2aError(RuntimeError):
    """A fail-closed transport, registry, or payload error."""


@dataclasses.dataclass(frozen=True)
class AgentRecord:
    """One fleet identity from the shared registry."""

    name: str
    port: int
    token: str = dataclasses.field(repr=False)
    generation: str = ""

    @property
    def base_url(self) -> str:
        return f"http://{BIND_HOST}:{self.port}"


def _utc_now() -> str:
    return dt.datetime.now(dt.UTC).isoformat(timespec="milliseconds")


def _atomic_json(path: Path, payload: dict[str, Any]) -> None:
    tmp = path.with_suffix(f".{os.getpid()}.tmp")
    try:
        with tmp.open("w") as handle:
            handle.write(
                json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True)
            )
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(tmp, stat.S_IRUSR | stat.S_IWUSR)
        os.replace(tmp, path)
    finally:
        tmp.unlink(missing_ok=True)


@contextlib.contextmanager
def _registry_lock(state_dir: Path) -> Iterator[None]:
    """Serialize registry and liveness read-modify-write operations."""

    state_dir.mkdir(parents=True, exist_ok=True)
    lock_path = state_dir / ".registry.lock"
    with lock_path.open("a+") as handle:
        os.chmod(lock_path, stat.S_IRUSR | stat.S_IWUSR)
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def _registry_payload(path: Path) -> dict[str, Any]:
    if not path.is_file():
        return {"schema_version": SCHEMA_VERSION, "agents": {}}
    payload = json.loads(path.read_text())
    if payload.get("schema_version") != SCHEMA_VERSION:
        raise A2aError(f"unsupported registry schema {payload.get('schema_version')!r}")
    agents = payload.get("agents")
    if not isinstance(agents, dict):
        raise A2aError("registry agents must be an object")
    return payload


def _validate_agent_name(name: str) -> None:
    if not IDENTITY_RE.match(name):
        raise A2aError(f"invalid agent name {name!r}")


def _validate_port(port: int) -> None:
    if isinstance(port, bool) or not isinstance(port, int) or not 1 <= port <= 65535:
        raise A2aError(f"invalid agent port {port!r}; expected integer in 1..65535")


def load_registry(state_dir: Path) -> dict[str, AgentRecord]:
    path = state_dir / "agents.json"
    if not path.is_file():
        raise A2aError(f"registry {path} missing; run the init command first")
    payload = _registry_payload(path)
    records: dict[str, AgentRecord] = {}
    for name, entry in payload.get("agents", {}).items():
        _validate_agent_name(name)
        if not isinstance(entry, dict):
            raise A2aError(f"agent {name!r} entry must be an object")
        token = entry.get("token")
        if not isinstance(token, str) or len(token) < 16:
            raise A2aError(f"agent {name!r} has no usable token")
        port = entry.get("port")
        if isinstance(port, bool) or not isinstance(port, int):
            raise A2aError(f"agent {name!r} has no usable port")
        _validate_port(port)
        generation = entry.get("generation")
        if generation is None:
            generation = hashlib.sha256(
                f"legacy\0{name}\0{port}\0{token}".encode()
            ).hexdigest()[:32]
        if not isinstance(generation, str) or not generation:
            raise A2aError(f"agent {name!r} has no usable generation")
        records[name] = AgentRecord(
            name=name,
            port=port,
            token=token,
            generation=generation,
        )
    if not records:
        raise A2aError(f"registry {path} lists no agents")
    return records


def _select_registration_port(
    agents: dict[str, Any], name: str, requested_port: int | None
) -> tuple[int, dict[str, Any] | None]:
    _validate_agent_name(name)
    existing = agents.get(name)
    if existing is not None and not isinstance(existing, dict):
        raise A2aError(f"agent {name!r} entry must be an object")
    if existing is not None:
        existing_port = existing.get("port")
        if not isinstance(existing_port, int) or isinstance(existing_port, bool):
            raise A2aError(f"agent {name!r} has no usable port")
        selected_port = existing_port if requested_port is None else requested_port
        if requested_port is not None and requested_port != existing_port:
            raise A2aError(
                f"agent {name!r} already uses port {existing_port}; "
                f"refusing requested port {requested_port}"
            )
    else:
        selected_port = (
            requested_port if requested_port is not None else KNOWN_AGENTS.get(name)
        )
        if selected_port is None:
            raise A2aError(
                f"unknown agent {name!r} requires --port; known port catalog: "
                f"{sorted(KNOWN_AGENTS)}"
            )
    _validate_port(selected_port)
    collision = next(
        (
            other
            for other, entry in agents.items()
            if other != name
            and isinstance(entry, dict)
            and entry.get("port") == selected_port
        ),
        None,
    )
    if collision is not None:
        raise A2aError(
            f"port {selected_port} is already assigned to agent {collision!r}"
        )
    return selected_port, existing


def _serve_port(state_dir: Path, name: str, port: int | None) -> int:
    """Resolve the port without mutating the registry."""

    state_dir.mkdir(parents=True, exist_ok=True)
    os.chmod(state_dir, stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
    with _registry_lock(state_dir):
        payload = _registry_payload(state_dir / "agents.json")
        selected_port, _ = _select_registration_port(payload["agents"], name, port)
        return selected_port


def init_registry(
    state_dir: Path,
    name: str | None = None,
    port: int | None = None,
    *,
    renew_generation: bool = False,
) -> dict[str, AgentRecord]:
    """Create one requested identity and preserve existing entries."""

    state_dir.mkdir(parents=True, exist_ok=True)
    os.chmod(state_dir, stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
    path = state_dir / "agents.json"
    with _registry_lock(state_dir):
        payload = _registry_payload(path)
        agents: dict[str, Any] = payload["agents"]
        if name is None and port is not None:
            raise A2aError("--port requires --name")
        if name is not None:
            selected_port, existing = _select_registration_port(agents, name, port)
            if existing is not None:
                token = existing.get("token")
                if not isinstance(token, str) or len(token) < 16:
                    token = secrets.token_urlsafe(24)
                generation = existing.get("generation")
            else:
                token = secrets.token_urlsafe(24)
                generation = None
            if renew_generation or not isinstance(generation, str) or not generation:
                generation = secrets.token_hex(16)
            agents[name] = {
                "port": selected_port,
                "token": token,
                "generation": generation,
            }
        _atomic_json(path, payload)
        if not agents:
            return {}
        return load_registry(state_dir)


def fetch_card(record: AgentRecord, timeout: float) -> dict[str, Any]:
    response = httpx.get(
        f"{record.base_url}{AGENT_CARD_WELL_KNOWN_PATH}",
        timeout=timeout,
    )
    if response.status_code != 200:
        raise A2aError(f"card fetch returned HTTP {response.status_code}")
    card = response.json()
    if card.get("name") != record.name:
        raise A2aError(
            f"card name {card.get('name')!r} does not match registry {record.name!r}"
        )
    return card


def _registration_fingerprint(record: AgentRecord) -> str:
    return hashlib.sha256(
        f"{record.name}\0{record.port}\0{record.generation}".encode()
    ).hexdigest()


def probe_peer(item: tuple[str, AgentRecord]) -> dict[str, Any]:
    name, record = item
    registration_fingerprint = _registration_fingerprint(record)
    try:
        card = fetch_card(record, timeout=PROBE_TIMEOUT_S)
        return {
            "name": name,
            "port": record.port,
            "registration_fingerprint": registration_fingerprint,
            "status": "up",
            "card_version": card.get("version"),
            "skills": [s.get("id") for s in card.get("skills", [])],
        }
    except (A2aError, httpx.HTTPError) as exc:
        return {
            "name": name,
            "port": record.port,
            "registration_fingerprint": registration_fingerprint,
            "status": "down",
            "reason": str(exc)[:200],
        }


def _liveness_payload(path: Path) -> dict[str, Any]:
    if not path.is_file():
        return {"schema_version": LIVENESS_SCHEMA_VERSION, "agents": {}}
    payload = json.loads(path.read_text())
    if payload.get("schema_version") != LIVENESS_SCHEMA_VERSION:
        raise A2aError(f"unsupported liveness schema {payload.get('schema_version')!r}")
    agents = payload.get("agents")
    if not isinstance(agents, dict):
        raise A2aError("liveness agents must be an object")
    return payload


def _record_probe_results(
    state_dir: Path, results: list[dict[str, Any]]
) -> list[dict[str, Any]]:
    """Persist consecutive TCP/card probe failures outside the identity registry."""

    path = state_dir / "liveness.json"
    with _registry_lock(state_dir):
        records = load_registry(state_dir)
        payload = _liveness_payload(path)
        states: dict[str, Any] = payload["agents"]
        updated: list[dict[str, Any]] = []
        now = _utc_now()
        for result in results:
            name = str(result["name"])
            current = records.get(name)
            if current is None or result.get(
                "registration_fingerprint"
            ) != _registration_fingerprint(current):
                updated.append(
                    {
                        **result,
                        "status": "stale",
                        "reason": "registration changed while probe was in flight",
                        "consecutive_failures": 0,
                    }
                )
                continue
            previous = states.get(name, {})
            if not isinstance(previous, dict):
                previous = {}
            previous_failures = previous.get("consecutive_failures", 0)
            if not isinstance(previous_failures, int) or previous_failures < 0:
                previous_failures = 0
            failures = 0 if result.get("status") == "up" else previous_failures + 1
            states[name] = {
                "consecutive_failures": failures,
                "last_probe_at": now,
                "registration_fingerprint": result["registration_fingerprint"],
            }
            updated.append({**result, "consecutive_failures": failures})
        payload["agents"] = {name: states[name] for name in records if name in states}
        _atomic_json(path, payload)
        return updated


def list_peers(state_dir: Path) -> list[dict[str, Any]]:
    records = load_registry(state_dir)
    with ThreadPoolExecutor(max_workers=8) as pool:
        results = list(pool.map(probe_peer, sorted(records.items())))
    return _record_probe_results(state_dir, results)
