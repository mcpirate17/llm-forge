"""Private registry and liveness primitives for the local A2A transport."""

from __future__ import annotations

import dataclasses
import datetime as dt
import hashlib
import json
import os
import re
import stat
import subprocess
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


def init_registry(
    state_dir: Path,
    name: str | None = None,
    port: int | None = None,
    *,
    renew_generation: bool = False,
) -> dict[str, AgentRecord]:
    """Create one identity through the native registry and return the public records."""

    args: list[str] = []
    if name is not None:
        args.extend(("--name", name))
    if port is not None:
        args.extend(("--port", str(port)))
    if renew_generation:
        args.append("--renew-generation")
    _run_native_registry(state_dir, "init", args)
    path = state_dir / "agents.json"
    if not _registry_payload(path)["agents"]:
        return {}
    return load_registry(state_dir)


def _run_native_registry(state_dir: Path, action: str, args: list[str]) -> Any:
    from conductor.a2a_delivery import _forge_binary

    command = [
        str(_forge_binary()),
        "mailbox",
        "--state-dir",
        str(state_dir),
        action,
        *args,
    ]
    try:
        completed = subprocess.run(command, text=True, capture_output=True, check=False)
    except OSError as exc:
        raise A2aError(f"cannot run native A2A {action}: {exc}") from exc
    if completed.returncode:
        raise A2aError(completed.stderr.strip() or f"native A2A {action} failed")
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise A2aError(f"invalid native A2A {action} response: {exc}") from exc


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


def list_peers(state_dir: Path) -> list[dict[str, Any]]:
    result = _run_native_registry(state_dir, "peers", [])
    if not isinstance(result, list) or any(
        not isinstance(item, dict) for item in result
    ):
        raise A2aError("invalid native A2A peers response")
    return result
