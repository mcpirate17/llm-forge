#!/usr/bin/env python3
"""Behavior-backed workspace reliability matrix with fail-closed receipts."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import shlex
import shutil
import subprocess
import sys
import urllib.error
import urllib.request
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Final, Iterable

from conductor.active_state import save_active_state
from conductor.audit_root import (
    AuditRootError,
    print_audit_provenance,
    resolve_audit_root,
)
from conductor.candidate_review.model import write_json_atomic
from conductor.candidate_review.ownership import load_claims
from conductor.local_ai_policy import CLERK_SYSTEM_PROMPT
from conductor.http_transport import open_http
from conductor import workspace_launcher_smokes as _launcher_smokes
from conductor import workspace_runtime_support as _runtime_support
from conductor.workspace_runtime_types import CellReceipt, LauncherSpec, ReceiptStatus

ROOT: Final[Path] = Path(__file__).resolve().parents[1]
# Relative to --root (see main()), not to Path(__file__) -- a caller in a
# different worktree must not write receipts into some other checkout.
DEFAULT_OUTPUT: Final[Path] = Path("research/reports/workspace_reliability_20260823")
EMBED_MODEL: Final[str] = "qwen3-embed-cpu"
CLERK_MODEL: Final[str] = "qwen3.5:9b"
CLERK_NUM_CTX: Final[int] = 2048
CLERK_NUM_GPU: Final[int] = 99
CLERK_NUM_PREDICT: Final[int] = 32
PROHIBITED_MODEL_FRAGMENTS: Final[tuple[str, ...]] = ("qwen3.8", "27b")
NOVEL_GPU_CLAIM_PATH_PREFIXES: Final[tuple[str, ...]] = (
    "component_fab/",
    "research/scientist/",
    "research/synthesis/",
    "research/tools/",
)
NOVEL_GPU_CLAIM_SIGNALS: Final[tuple[str, ...]] = (
    "avo",
    "cuda",
    "gpu",
    "optimizer",
    "throughput",
    "training",
)
NON_RESEARCH_GPU_PROCESS_FRAGMENTS: Final[tuple[str, ...]] = (
    "gnome-remote-desktop",
    "gnome-shell",
    "xorg",
)
REQUIRED_LAUNCHERS: Final[tuple[str, ...]] = (
    "codex",
    "claude",
    "glm",
    "qwen",
    "grok",
)
MAX_LAUNCHER_CALLS: Final[int] = 5
MAX_SECONDS_PER_CALL: Final[int] = 15 * 60
MAX_REPORTED_TOKENS: Final[int] = 250_000
MAX_LOG_BYTES: Final[int] = 2_000_000


@dataclass(frozen=True, slots=True)
class WorkspaceReceipt:
    schema_version: int
    generated_at: str
    status: ReceiptStatus
    cells: tuple[CellReceipt, ...]
    provenance: dict[str, Any]

    def to_dict(self) -> dict[str, Any]:
        payload = asdict(self)
        payload["status"] = self.status.value
        for cell in payload["cells"]:
            cell["status"] = str(cell["status"])
        return payload


@dataclass(frozen=True, slots=True)
class GpuComputeProcess:
    pid: int
    process_name: str
    used_memory_mib: int


@dataclass(frozen=True, slots=True)
class ClerkGpuPreflight:
    ready: bool
    blocking_claim_ids: tuple[str, ...]
    blocking_processes: tuple[GpuComputeProcess, ...]
    loaded_models: tuple[str, ...]

    def to_evidence(self) -> dict[str, Any]:
        return {
            "ready": self.ready,
            "blocking_claim_ids": list(self.blocking_claim_ids),
            "blocking_processes": [
                asdict(process) for process in self.blocking_processes
            ],
            "loaded_models": list(self.loaded_models),
        }


@dataclass(frozen=True, slots=True)
class ClerkAttempt:
    response: dict[str, Any] | None
    resident_processes: str
    after_processes: str
    stop_returncode: int
    request_error: str


def _sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _sha256_path(path: Path) -> str:
    return _sha256_bytes(path.read_bytes())


def aggregate_status(cells: Iterable[CellReceipt]) -> ReceiptStatus:
    required = [cell for cell in cells if cell.required]
    if any(cell.status is ReceiptStatus.FAIL_CLOSED for cell in required):
        return ReceiptStatus.FAIL_CLOSED
    if any(cell.status is ReceiptStatus.NOT_READY for cell in required):
        return ReceiptStatus.NOT_READY
    return ReceiptStatus.PASS


def _run(
    argv: list[str],
    *,
    cwd: Path = ROOT,
    timeout: float = 30.0,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv,
        cwd=cwd,
        env=env,
        text=True,
        capture_output=True,
        timeout=timeout,
        check=False,
    )


def check_active_state(repo: Path = ROOT) -> CellReceipt:
    try:
        state_path = repo / "conductor" / "active_state.json"
        state = save_active_state(state_path)
        claims, digest = load_claims(repo)
    except (OSError, RuntimeError, TypeError, ValueError) as exc:
        return CellReceipt(
            "active-state-live-claims",
            ReceiptStatus.FAIL_CLOSED,
            f"state or live claim source failed: {exc}",
        )
    now = datetime.now(timezone.utc)
    live_ids = sorted(claim.claim_id for claim in claims if claim.active(now))
    cached_ids = sorted(str(claim.get("claim_id")) for claim in state.active_claims)
    age = (now - datetime.fromisoformat(state.last_updated)).total_seconds()
    ok = live_ids == cached_ids and -60.0 <= age <= 30.0
    return CellReceipt(
        "active-state-live-claims",
        ReceiptStatus.PASS if ok else ReceiptStatus.FAIL_CLOSED,
        "fresh atomic cache agrees with live claim store"
        if ok
        else f"cached={cached_ids}, live={live_ids}, age={age:.3f}s",
        evidence={
            "active_state_sha256": _sha256_path(state_path),
            "claim_store_sha256": digest,
            "claim_ids": live_ids,
            "age_seconds": round(age, 6),
        },
    )


def _load_json(path: Path) -> dict[str, Any]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError(f"expected JSON object: {path}")
    return payload


def check_hook_configs(repo: Path = ROOT) -> CellReceipt:
    paths = {
        "codex": repo / ".codex" / "hooks.json",
        "claude": repo / ".claude" / "settings.json",
        "qwen": repo / ".qwen" / "settings.json",
        "grok": repo / ".grok" / "hooks" / "workspace.json",
    }
    expected = {
        "codex": (
            "pre-edit.sh",
            "crg_gate.py verify",
            '"Read"',
            "GOVERNANCE_OWNER",
        ),
        "claude": (
            "pre-edit.sh",
            "crg_gate.py verify",
            '"Read"',
            "GOVERNANCE_OWNER",
        ),
        "qwen": (
            "pre-edit.sh",
            "crg_gate.py verify",
            "read_file",
            "run_shell_command",
        ),
        "grok": (
            "pre-edit.sh",
            "crg_gate.py verify",
            "read_file",
            "run_shell_command",
        ),
    }
    errors: list[str] = []
    hashes: dict[str, str] = {}
    for launcher, path in paths.items():
        try:
            payload = _load_json(path)
            serialized = json.dumps(payload.get("hooks", {}), sort_keys=True)
        except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as exc:
            errors.append(f"{launcher}:{exc}")
            continue
        hashes[launcher] = _sha256_path(path)
        missing = [
            fragment for fragment in expected[launcher] if fragment not in serialized
        ]
        if missing:
            errors.append(f"{launcher}:missing={missing}")
    return CellReceipt(
        "hook-config-contract",
        ReceiptStatus.FAIL_CLOSED if errors else ReceiptStatus.PASS,
        "; ".join(errors)
        if errors
        else "native configs bind read, shell, graph, and owner gates",
        evidence={"config_sha256": hashes},
    )


def _hook_call(
    payload: dict[str, Any],
    program: Path,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(program)],
        input=json.dumps(payload),
        text=True,
        capture_output=True,
        timeout=5,
        check=False,
    )


def _grok_inspect_argv() -> list[str]:
    """Return the Grok inspection command, with a deterministic test seam."""
    configured = os.environ.get("GROK_INSPECT_COMMAND")
    if configured:
        command = shlex.split(configured)
        if command:
            return command
    return ["grok", "inspect", "--json"]


def _check_hook_sources(
    root: Path, failures: list[str], evidence: dict[str, Any]
) -> None:
    """Validate each native raw guard and every referenced hook program."""
    _runtime_support.check_hook_sources(
        root,
        failures,
        evidence,
        hook_call=_hook_call,
        run_command=_run,
    )


def _check_hook_noops(
    root: Path, failures: list[str], evidence: dict[str, Any]
) -> None:
    """Exercise destructive-command denials and bounded post-hook no-ops."""
    _runtime_support.check_hook_noops(
        root,
        failures,
        evidence,
        hook_call=_hook_call,
    )


def _check_preamble_and_grok(
    root: Path, failures: list[str], evidence: dict[str, Any]
) -> None:
    """Validate the injected preamble and Grok's trusted project hook discovery."""
    _runtime_support.check_preamble_and_grok(
        root,
        failures,
        evidence,
        run_command=_run,
        sha256_bytes=_sha256_bytes,
        grok_argv=_grok_inspect_argv,
    )


def check_hook_programs(root: Path = ROOT) -> CellReceipt:
    """Exercise the hook suites rooted at ``root`` against the control matrix."""
    cases: tuple[tuple[str, dict[str, Any], bool], ...] = (
        (
            "codex-read-deny",
            {"tool_name": "Read", "tool_input": {"file_path": ".current_work.md"}},
            True,
        ),
        (
            "grok-read-deny",
            {
                "hookEventName": "pre_tool_use",
                "toolName": "read_file",
                "toolInput": {"filePath": ".current_work.md"},
            },
            True,
        ),
        (
            "shell-read-deny",
            {
                "tool_name": "Bash",
                "tool_input": {"command": "sed -n '1,10p' .current_work.md"},
            },
            True,
        ),
        (
            "safe-read-allow",
            {"tool_name": "Read", "tool_input": {"file_path": "README.md"}},
            False,
        ),
        (
            "handoff-allow",
            {
                "tool_name": "Bash",
                "tool_input": {
                    "command": "python -m conductor.handoff append --owner x --title y --body z"
                },
            },
            False,
        ),
    )
    failures: list[str] = []
    evidence: dict[str, Any] = {}
    program = root / ".codex" / "hooks" / "pre-edit.sh"
    for name, payload, expect_deny in cases:
        result = _hook_call(payload, program)
        denied = "BLOCKED:" in result.stdout
        evidence[name] = {"returncode": result.returncode, "denied": denied}
        if result.returncode != 0 or denied is not expect_deny:
            failures.append(name)
    _check_hook_sources(root, failures, evidence)
    _check_hook_noops(root, failures, evidence)
    _check_preamble_and_grok(root, failures, evidence)
    return CellReceipt(
        "hook-program-controls",
        ReceiptStatus.FAIL_CLOSED if failures else ReceiptStatus.PASS,
        f"failed controls: {failures}"
        if failures
        else "all referenced hook programs passed syntax and bounded controls",
        evidence=evidence,
    )


def check_launcher_programs() -> CellReceipt:
    versions: dict[str, str] = {}
    missing: list[str] = []
    for launcher in REQUIRED_LAUNCHERS:
        executable = shutil.which(launcher)
        if executable is None:
            missing.append(launcher)
            continue
        result = _run([executable, "--version"], timeout=10)
        if result.returncode != 0:
            missing.append(f"{launcher}:exit={result.returncode}")
        else:
            versions[launcher] = (
                (result.stdout or result.stderr).strip().splitlines()[0]
            )
    return CellReceipt(
        "launcher-programs",
        ReceiptStatus.NOT_READY if missing else ReceiptStatus.PASS,
        f"unavailable={missing}" if missing else "all five launcher binaries responded",
        evidence={"versions": versions},
    )


def _http_json(
    url: str,
    *,
    payload: dict[str, Any] | None = None,
    timeout: float = 30.0,
) -> dict[str, Any]:
    data = None if payload is None else json.dumps(payload).encode()
    request = urllib.request.Request(
        url,
        data=data,
        headers={"Content-Type": "application/json"},
        method="POST" if data is not None else "GET",
    )
    with open_http(urllib.request.urlopen, request, timeout=timeout) as response:
        result = json.loads(response.read().decode())
    if not isinstance(result, dict):
        raise ValueError(f"expected JSON object from {url}")
    return result


def _ollama_ps() -> str:
    result = _run(["ollama", "ps"], timeout=15)
    return (result.stdout + result.stderr).strip()


def _ollama_model_rows(raw: str) -> tuple[str, ...]:
    lines = raw.splitlines()
    if not lines or not lines[0].lstrip().startswith("NAME"):
        raise RuntimeError(f"unexpected ollama ps output: {raw!r}")
    return tuple(line for line in lines[1:] if line.strip())


def _nonnegative_int(payload: dict[str, Any], key: str) -> int:
    value = payload.get(key)
    return value if type(value) is int and value >= 0 else -1


def _gpu_compute_processes() -> tuple[GpuComputeProcess, ...]:
    result = _run(
        [
            "nvidia-smi",
            "--query-compute-apps=pid,process_name,used_memory",
            "--format=csv,noheader,nounits",
        ],
        timeout=10,
    )
    if result.returncode != 0:
        raise RuntimeError(
            "nvidia-smi compute-process query failed: "
            f"exit={result.returncode}, stderr={result.stderr.strip()!r}"
        )
    processes: list[GpuComputeProcess] = []
    for line in result.stdout.splitlines():
        if not line.strip():
            continue
        parts = [part.strip() for part in line.rsplit(",", maxsplit=2)]
        if len(parts) != 3:
            raise RuntimeError(f"unexpected nvidia-smi process row: {line!r}")
        try:
            pid = int(parts[0])
            used_memory_mib = int(parts[2])
        except ValueError as exc:
            raise RuntimeError(f"invalid nvidia-smi process row: {line!r}") from exc
        processes.append(
            GpuComputeProcess(
                pid=pid,
                process_name=parts[1],
                used_memory_mib=used_memory_mib,
            )
        )
    return tuple(processes)


def _claim_reserves_novel_gpu(claim: Any) -> bool:
    paths = tuple(str(path).casefold() for path in claim.paths)
    if not any(path.startswith(NOVEL_GPU_CLAIM_PATH_PREFIXES) for path in paths):
        return False
    text = " ".join((str(claim.owner), str(claim.justification), *paths)).casefold()
    return any(signal in text for signal in NOVEL_GPU_CLAIM_SIGNALS)


def clerk_gpu_preflight(repo: Path = ROOT) -> ClerkGpuPreflight:
    """Refuse a 9B load while novel research may own the accelerator."""

    now = datetime.now(timezone.utc)
    claims, _digest = load_claims(repo)
    blocking_claims = tuple(
        sorted(
            claim.claim_id
            for claim in claims
            if claim.active(now) and _claim_reserves_novel_gpu(claim)
        )
    )
    blocking_processes = tuple(
        process
        for process in _gpu_compute_processes()
        if not any(
            fragment in process.process_name.casefold()
            for fragment in NON_RESEARCH_GPU_PROCESS_FRAGMENTS
        )
    )
    loaded_models = _ollama_model_rows(_ollama_ps())
    return ClerkGpuPreflight(
        ready=not blocking_claims and not blocking_processes and not loaded_models,
        blocking_claim_ids=blocking_claims,
        blocking_processes=blocking_processes,
        loaded_models=loaded_models,
    )


def check_embedding_canary() -> CellReceipt:
    try:
        health = _http_json("http://127.0.0.1:7317/health", timeout=5)
        response = _http_json(
            "http://127.0.0.1:7317/v1/embeddings",
            payload={"model": EMBED_MODEL, "input": ["workspace reliability canary"]},
            timeout=120,
        )
    except (OSError, TimeoutError, ValueError, urllib.error.URLError) as exc:
        return CellReceipt(
            "embedding-canary", ReceiptStatus.NOT_READY, f"embedding unavailable: {exc}"
        )
    rows = response.get("data")
    vector = rows[0].get("embedding") if isinstance(rows, list) and rows else None
    finite = isinstance(vector, list) and all(
        isinstance(value, (int, float)) and math.isfinite(float(value))
        for value in vector
    )
    processes = _ollama_ps()
    prohibited = [
        fragment
        for fragment in PROHIBITED_MODEL_FRAGMENTS
        if fragment in processes.casefold()
    ]
    ok = (
        health.get("ok") is True
        and health.get("model") == EMBED_MODEL
        and health.get("num_ctx") == 2048
        and health.get("keep_alive") == 0
        and finite
        and len(vector) == 1024
        and EMBED_MODEL not in processes
        and not prohibited
    )
    return CellReceipt(
        "embedding-canary",
        ReceiptStatus.PASS if ok else ReceiptStatus.FAIL_CLOSED,
        "finite 1024-d vector with bound policy and unload"
        if ok
        else "embedding policy or unload failed",
        evidence={
            "model": health.get("model"),
            "num_ctx": health.get("num_ctx"),
            "num_gpu": health.get("num_gpu"),
            "keep_alive": health.get("keep_alive"),
            "dimension": len(vector) if isinstance(vector, list) else None,
            "finite": finite,
            "prohibited_loaded": prohibited,
        },
    )


def check_retrievers() -> CellReceipt:
    query = "workspace reliability hooks embedding graph active state"
    commands = {
        "kb": ["python", "-m", "conductor.kb_retrieve", "query", query, "--top-k", "5"],
        "memory": [
            "python",
            "-m",
            "conductor.memory_index",
            "query",
            query,
            "--top-k",
            "8",
        ],
    }
    evidence: dict[str, Any] = {}
    unavailable: list[str] = []
    invalid: list[str] = []
    for name, argv in commands.items():
        try:
            result = _run(argv, timeout=120)
        except subprocess.TimeoutExpired:
            unavailable.append(f"{name}:timeout")
            continue
        if result.returncode != 0:
            unavailable.append(f"{name}:exit={result.returncode}")
            continue
        try:
            payload = json.loads(result.stdout)
        except json.JSONDecodeError:
            invalid.append(f"{name}:malformed")
            continue
        if not isinstance(payload, list) or not payload:
            invalid.append(f"{name}:empty")
            continue
        evidence[name] = {
            "hit_count": len(payload),
            "top_path": payload[0].get("path")
            if isinstance(payload[0], dict)
            else None,
            "stdout_sha256": _sha256_bytes(result.stdout.encode()),
        }
    status = (
        ReceiptStatus.FAIL_CLOSED
        if invalid
        else ReceiptStatus.NOT_READY
        if unavailable
        else ReceiptStatus.PASS
    )
    return CellReceipt(
        "retriever-runtime",
        status,
        f"unavailable={unavailable}, invalid={invalid}"
        if unavailable or invalid
        else "both retrievers returned non-empty JSON",
        evidence=evidence,
    )


def _token_total(value: Any) -> int:
    if isinstance(value, dict):
        total = value.get("total_tokens")
        if isinstance(total, (int, float)):
            return int(total)
        candidates: list[int] = []
        for input_key, output_key in (
            ("input_tokens", "output_tokens"),
            ("prompt_tokens", "completion_tokens"),
        ):
            input_tokens = value.get(input_key)
            output_tokens = value.get(output_key)
            if isinstance(input_tokens, (int, float)) and isinstance(
                output_tokens, (int, float)
            ):
                # Cached and reasoning counts are breakdowns, not extra tokens.
                candidates.append(int(input_tokens) + int(output_tokens))
        candidates.extend(_token_total(item) for item in value.values())
        return max(candidates, default=0)
    if isinstance(value, list):
        return max((_token_total(item) for item in value), default=0)
    return 0


def extract_reported_tokens(output: str) -> int:
    totals: list[int] = []
    for line in output.splitlines():
        try:
            totals.append(_token_total(json.loads(line)))
        except json.JSONDecodeError:
            continue
    # Each launcher emits a cumulative terminal usage object. Taking the maximum
    # avoids summing intermediate snapshots and repeated final summaries.
    return max(totals, default=0)


def launcher_specs(root: Path = ROOT) -> tuple[LauncherSpec, ...]:
    return _launcher_smokes.launcher_specs(root, REQUIRED_LAUNCHERS)


def _launcher_runtime() -> _launcher_smokes.LauncherRuntime:
    return _launcher_smokes.LauncherRuntime(
        run=_run,
        token_counter=extract_reported_tokens,
        sha256_path=_sha256_path,
        sha256_bytes=_sha256_bytes,
        write_json=write_json_atomic,
        max_calls=MAX_LAUNCHER_CALLS,
        seconds_per_call=MAX_SECONDS_PER_CALL,
        max_reported_tokens=MAX_REPORTED_TOKENS,
        max_log_bytes=MAX_LOG_BYTES,
    )


def run_launcher_smokes(output_dir: Path, *, root: Path = ROOT) -> CellReceipt:
    return _launcher_smokes.run_launcher_smokes(
        output_dir,
        launcher_specs(root),
        _launcher_runtime(),
    )


def reconcile_launcher_logs(output_dir: Path) -> CellReceipt:
    """Recompute usage from preserved logs without making another model call."""
    return _launcher_smokes.reconcile_launcher_logs(
        output_dir,
        REQUIRED_LAUNCHERS,
        _launcher_runtime(),
    )


def reconcile_receipt(output_dir: Path, repo: Path = ROOT) -> dict[str, Any]:
    """Recompute bounded local evidence without making another model call."""
    path = output_dir / "receipt.json"
    payload = _load_json(path)
    archive = output_dir / "receipt.pre_reconcile.json"
    if not archive.exists():
        write_json_atomic(archive, payload)
    archive_sha256 = _sha256_path(archive)
    replacements = {
        cell.cell_id: cell
        for cell in (
            check_hook_configs(repo),
            check_hook_programs(repo),
            reconcile_launcher_logs(output_dir),
        )
    }
    cells = payload.get("cells")
    if not isinstance(cells, list):
        raise ValueError("receipt cells must be a list")
    present_ids = {str(cell.get("cell_id")) for cell in cells if isinstance(cell, dict)}
    if "launcher-real-smokes" not in present_ids:
        raise ValueError("launcher-real-smokes cell missing")
    replacements = {
        cell_id: cell
        for cell_id, cell in replacements.items()
        if cell_id in present_ids
    }
    for index, cell in enumerate(cells):
        cell_id = cell.get("cell_id") if isinstance(cell, dict) else None
        replacement = replacements.pop(str(cell_id), None)
        if replacement is None:
            continue
        replacement_dict = asdict(replacement)
        replacement_dict["status"] = replacement.status.value
        cells[index] = replacement_dict
    if replacements:
        raise ValueError(f"receipt replacement failed: {sorted(replacements)}")
    required_statuses = [
        ReceiptStatus(cell["status"])
        for cell in cells
        if isinstance(cell, dict) and cell.get("required", True)
    ]
    payload["status"] = aggregate_status(
        CellReceipt("cell", status, "reconciled") for status in required_statuses
    ).value
    provenance = payload.setdefault("provenance", {})
    if not isinstance(provenance, dict):
        raise ValueError("receipt provenance must be an object")
    provenance.update(
        {
            "launcher_usage_reconciled_at": datetime.now(timezone.utc).isoformat(),
            "hook_programs_rechecked": True,
            "pre_reconcile_receipt_sha256": archive_sha256,
            "pre_reconcile_receipt": str(archive),
        }
    )
    write_json_atomic(path, payload)
    return payload


def _reconcile_cell(
    output_dir: Path,
    replacement: CellReceipt,
    *,
    archive_name: str,
    archive_provenance_prefix: str,
    provenance_update: dict[str, Any],
) -> dict[str, Any]:
    path = output_dir / "receipt.json"
    payload = _load_json(path)
    archive = output_dir / archive_name
    if not archive.exists():
        write_json_atomic(archive, payload)
    cells = payload.get("cells")
    if not isinstance(cells, list):
        raise ValueError("receipt cells must be a list")
    found = False
    for index, cell in enumerate(cells):
        if not isinstance(cell, dict) or cell.get("cell_id") != replacement.cell_id:
            continue
        replacement_dict = asdict(replacement)
        replacement_dict["status"] = replacement.status.value
        cells[index] = replacement_dict
        found = True
        break
    if not found:
        raise ValueError(f"{replacement.cell_id} cell missing")
    required_statuses = [
        ReceiptStatus(cell["status"])
        for cell in cells
        if isinstance(cell, dict) and cell.get("required", True)
    ]
    payload["status"] = aggregate_status(
        CellReceipt("cell", status, "reconciled") for status in required_statuses
    ).value
    provenance = payload.setdefault("provenance", {})
    if not isinstance(provenance, dict):
        raise ValueError("receipt provenance must be an object")
    provenance.update(
        {
            **provenance_update,
            f"pre_{archive_provenance_prefix}_receipt": str(archive),
            f"pre_{archive_provenance_prefix}_receipt_sha256": _sha256_path(archive),
        }
    )
    write_json_atomic(path, payload)
    return payload


def reconcile_graph_evidence(
    output_dir: Path,
    graph_evidence: Path,
) -> dict[str, Any]:
    """Replace only the graph cell while preserving prior expensive evidence."""

    return _reconcile_cell(
        output_dir,
        load_graph_evidence(graph_evidence),
        archive_name="receipt.pre_graph_reconcile.json",
        archive_provenance_prefix="graph_reconcile",
        provenance_update={
            "graph_evidence_reconciled_at": datetime.now(timezone.utc).isoformat(),
            "graph_evidence": str(graph_evidence),
            "graph_evidence_sha256": _sha256_path(graph_evidence),
        },
    )


def _clerk_schema() -> dict[str, Any]:
    return {
        "type": "object",
        "properties": {
            "status": {"type": "string", "enum": ["PASS"]},
            "cells": {"type": "integer", "const": 5},
        },
        "required": ["status", "cells"],
        "additionalProperties": False,
    }


def _clerk_payload(schema: dict[str, Any]) -> dict[str, Any]:
    schema_text = json.dumps(schema, sort_keys=True, separators=(",", ":"))
    return {
        "model": CLERK_MODEL,
        "messages": [
            {"role": "system", "content": CLERK_SYSTEM_PROMPT},
            {
                "role": "user",
                "content": (
                    "Return only compact JSON matching this schema exactly: "
                    f"{schema_text}. The only valid object is "
                    '{"status":"PASS","cells":5}.'
                ),
            },
        ],
        "format": schema,
        "stream": False,
        "think": False,
        "keep_alive": "30s",
        "options": {
            "num_ctx": CLERK_NUM_CTX,
            "num_gpu": CLERK_NUM_GPU,
            "num_predict": CLERK_NUM_PREDICT,
            "presence_penalty": 0,
            "seed": 0,
            "temperature": 0,
        },
    }


def _execute_clerk_canary(payload: dict[str, Any]) -> ClerkAttempt:
    response: dict[str, Any] | None = None
    resident_processes = ""
    request_error = ""
    try:
        response = _http_json(
            "http://127.0.0.1:11434/api/chat",
            payload=payload,
            timeout=180,
        )
        resident_processes = _ollama_ps()
    except (OSError, TimeoutError, ValueError, urllib.error.URLError) as exc:
        request_error = str(exc)
    finally:
        unload = _run(["ollama", "stop", CLERK_MODEL], timeout=30)
        after_processes = _ollama_ps()
    return ClerkAttempt(
        response=response,
        resident_processes=resident_processes,
        after_processes=after_processes,
        stop_returncode=unload.returncode,
        request_error=request_error,
    )


def _clerk_unavailable_evidence(
    preflight: ClerkGpuPreflight,
    attempt: ClerkAttempt,
) -> dict[str, Any]:
    try:
        after_rows = _ollama_model_rows(attempt.after_processes)
        unloaded = all(CLERK_MODEL not in row for row in after_rows)
    except RuntimeError:
        unloaded = False
    return {
        "preflight": preflight.to_evidence(),
        "request_error": attempt.request_error,
        "stop_returncode": attempt.stop_returncode,
        "unloaded": unloaded,
    }


def _adjudicate_clerk_attempt(
    preflight: ClerkGpuPreflight,
    attempt: ClerkAttempt,
) -> tuple[bool, dict[str, Any]]:
    if attempt.response is None:
        raise ValueError("cannot adjudicate a missing clerk response")
    response = attempt.response
    message = response.get("message")
    content = message.get("content") if isinstance(message, dict) else None
    thinking = message.get("thinking") if isinstance(message, dict) else None
    try:
        parsed = json.loads(content) if isinstance(content, str) else None
    except json.JSONDecodeError:
        parsed = None
    prompt_tokens = _nonnegative_int(response, "prompt_eval_count")
    eval_tokens = _nonnegative_int(response, "eval_count")
    tokens = prompt_tokens + eval_tokens
    try:
        resident_rows = _ollama_model_rows(attempt.resident_processes)
        after_rows = _ollama_model_rows(attempt.after_processes)
        ollama_ps_valid = True
    except RuntimeError:
        resident_rows = ()
        after_rows = ("invalid ollama ps output",)
        ollama_ps_valid = False
    gpu_resident = any(CLERK_MODEL in row and "GPU" in row for row in resident_rows)
    unloaded = all(CLERK_MODEL not in row for row in after_rows)
    prohibited = [
        fragment
        for fragment in PROHIBITED_MODEL_FRAGMENTS
        if fragment in attempt.resident_processes.casefold()
    ]
    generation_bounded = 0 < eval_tokens <= CLERK_NUM_PREDICT
    schema_valid = parsed == {"status": "PASS", "cells": 5}
    ok = (
        response.get("model") == CLERK_MODEL
        and schema_valid
        and response.get("done") is True
        and response.get("done_reason") == "stop"
        and not thinking
        and ollama_ps_valid
        and gpu_resident
        and attempt.stop_returncode == 0
        and unloaded
        and not prohibited
        and tokens > 0
        and generation_bounded
    )
    evidence = {
        "preflight": preflight.to_evidence(),
        "model": response.get("model"),
        "schema_valid": schema_valid,
        "reported_tokens": tokens,
        "prompt_eval_count": prompt_tokens,
        "eval_count": eval_tokens,
        "done_reason": response.get("done_reason"),
        "thinking_disabled": True,
        "thinking_suppressed": not thinking,
        "num_ctx": CLERK_NUM_CTX,
        "num_gpu": CLERK_NUM_GPU,
        "num_predict": CLERK_NUM_PREDICT,
        "generation_bounded": generation_bounded,
        "ollama_ps_valid": ollama_ps_valid,
        "gpu_resident": gpu_resident,
        "unloaded": unloaded,
        "stop_returncode": attempt.stop_returncode,
        "prohibited_loaded": prohibited,
        "response_sha256": _sha256_bytes(json.dumps(response, sort_keys=True).encode()),
    }
    return ok, evidence


def run_clerk_canary(output_dir: Path, *, root: Path = ROOT) -> CellReceipt:
    try:
        preflight = clerk_gpu_preflight(root)
    except (OSError, RuntimeError, TypeError, ValueError) as exc:
        return CellReceipt(
            "local-clerk-canary",
            ReceiptStatus.NOT_READY,
            f"9B clerk GPU preflight failed: {exc}",
        )
    if not preflight.ready:
        return CellReceipt(
            "local-clerk-canary",
            ReceiptStatus.NOT_READY,
            "9B clerk deferred because novel research or another model owns the GPU",
            evidence={"preflight": preflight.to_evidence()},
        )
    attempt = _execute_clerk_canary(_clerk_payload(_clerk_schema()))
    if attempt.request_error or attempt.response is None:
        evidence = _clerk_unavailable_evidence(preflight, attempt)
        write_json_atomic(output_dir / "local_clerk.json", evidence)
        return CellReceipt(
            "local-clerk-canary",
            ReceiptStatus.NOT_READY,
            f"9B clerk unavailable: {attempt.request_error}",
            evidence=evidence,
        )
    ok, evidence = _adjudicate_clerk_attempt(preflight, attempt)
    write_json_atomic(output_dir / "local_clerk.json", evidence)
    return CellReceipt(
        "local-clerk-canary",
        ReceiptStatus.PASS if ok else ReceiptStatus.FAIL_CLOSED,
        "schema-valid 9B GPU call completed and unloaded"
        if ok
        else "clerk invariant failed",
        evidence=evidence,
    )


def reconcile_clerk_evidence(output_dir: Path, *, root: Path = ROOT) -> dict[str, Any]:
    """Run and replace only the clerk cell, preserving launcher evidence."""

    return _reconcile_cell(
        output_dir,
        run_clerk_canary(output_dir, root=root),
        archive_name="receipt.pre_clerk_reconcile.json",
        archive_provenance_prefix="clerk_reconcile",
        provenance_update={
            "clerk_evidence_reconciled_at": datetime.now(timezone.utc).isoformat(),
            "clerk_evidence": str(output_dir / "local_clerk.json"),
        },
    )


def load_graph_evidence(path: Path | None) -> CellReceipt:
    if path is None or not path.is_file():
        return CellReceipt(
            "graph-semantic-runtime",
            ReceiptStatus.NOT_READY,
            "no graph evidence supplied",
        )
    try:
        payload = _load_json(path)
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as exc:
        return CellReceipt(
            "graph-semantic-runtime",
            ReceiptStatus.FAIL_CLOSED,
            f"graph evidence malformed: {exc}",
        )
    provider = payload.get("provider")
    backend_fingerprint = payload.get("backend_fingerprint")
    query_trace = payload.get("query_trace")
    live_nodes = payload.get("live_non_file_node_count")
    embedded_nodes = payload.get("embedded_node_count")
    ok = (
        isinstance(provider, str)
        and provider.startswith("workspace:")
        and isinstance(backend_fingerprint, str)
        and backend_fingerprint.startswith("sha256:")
        and isinstance(payload.get("model"), str)
        and bool(payload["model"])
        and isinstance(payload.get("dimension"), int)
        and payload["dimension"] > 0
        and isinstance(payload.get("paid"), bool)
        and payload.get("stored_provider") == provider
        and payload.get("search_mode") in {"semantic", "hybrid"}
        and isinstance(payload.get("result_count"), int)
        and payload["result_count"] > 0
        and isinstance(payload.get("node_count"), int)
        and payload["node_count"] > 0
        and isinstance(live_nodes, int)
        and live_nodes > 0
        and embedded_nodes == live_nodes
        and payload.get("missing_embedding_count") == 0
        and payload.get("mixed_provider_live_count") == 0
        and payload.get("orphan_embedding_count") == 0
        and payload.get("expected_result_found") is True
        and isinstance(query_trace, dict)
        and query_trace.get("provider_name") == provider
        and query_trace.get("backend_fingerprint") == backend_fingerprint
        and query_trace.get("purpose") == "query"
        and query_trace.get("vector_count") == 1
        and isinstance(query_trace.get("broker_calls"), int)
        and query_trace["broker_calls"] > 0
        and query_trace.get("dimension") == payload["dimension"]
        and query_trace.get("paid") == payload["paid"]
    )
    return CellReceipt(
        "graph-semantic-runtime",
        ReceiptStatus.PASS if ok else ReceiptStatus.FAIL_CLOSED,
        "fingerprint-bound semantic graph query covered the live graph"
        if ok
        else "graph fallback or mismatch",
        evidence={**payload, "source_sha256": _sha256_path(path)},
    )


def build_receipt(
    *,
    output_dir: Path,
    graph_evidence: Path | None,
    run_launchers: bool,
    run_clerk: bool,
    root: Path = ROOT,
) -> WorkspaceReceipt:
    output_dir.mkdir(parents=True, exist_ok=True)
    cells = [
        check_active_state(root),
        check_hook_configs(root),
        check_hook_programs(root),
        check_launcher_programs(),
        check_embedding_canary(),
        check_retrievers(),
        load_graph_evidence(graph_evidence),
        run_launcher_smokes(output_dir, root=root)
        if run_launchers
        else CellReceipt(
            "launcher-real-smokes", ReceiptStatus.NOT_READY, "launcher calls not run"
        ),
        run_clerk_canary(output_dir, root=root)
        if run_clerk
        else CellReceipt(
            "local-clerk-canary", ReceiptStatus.NOT_READY, "clerk call not run"
        ),
    ]
    head = _run(["git", "rev-parse", "HEAD"], timeout=10, cwd=root).stdout.strip()
    return WorkspaceReceipt(
        schema_version=1,
        generated_at=datetime.now(timezone.utc).isoformat(),
        status=aggregate_status(cells),
        cells=tuple(cells),
        provenance={
            "repo": str(root),
            "git_head": head,
            "claim_store_sha256": load_claims(root)[1],
            "allowed_models": [EMBED_MODEL, CLERK_MODEL],
            "prohibited_model_fragments": list(PROHIBITED_MODEL_FRAGMENTS),
            "launcher_call_budget": MAX_LAUNCHER_CALLS,
            "seconds_per_launcher_call": MAX_SECONDS_PER_CALL,
            "reported_token_budget": MAX_REPORTED_TOKENS,
        },
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        type=Path,
        default=DEFAULT_OUTPUT,
        help="Relative paths resolve against --root (or its default).",
    )
    parser.add_argument("--graph-evidence", type=Path)
    parser.add_argument("--run-launchers", action="store_true")
    parser.add_argument("--run-clerk", action="store_true")
    parser.add_argument(
        "--reconcile-launcher-logs",
        action="store_true",
        help="recompute launcher usage from preserved logs without model calls",
    )
    parser.add_argument(
        "--reconcile-graph-evidence",
        type=Path,
        help="replace only the graph cell from a durable evidence file",
    )
    parser.add_argument(
        "--reconcile-clerk",
        action="store_true",
        help="run and replace only the local clerk cell",
    )
    parser.add_argument(
        "--root",
        help=(
            "Repository tree to evaluate. Defaults to the Git worktree containing "
            "the current working directory, never the checkout that supplied "
            "the imported conductor module."
        ),
    )
    args = parser.parse_args(argv)

    try:
        root = resolve_audit_root(args.root)
    except AuditRootError as exc:
        print(f"ERROR: workspace-runtime-matrix: {exc}", file=sys.stderr)
        return 2
    print_audit_provenance("workspace-runtime-matrix", root)
    output_dir = root / args.output

    if args.reconcile_launcher_logs:
        payload = reconcile_receipt(output_dir, repo=root)
        payload["receipt"] = str(output_dir / "receipt.json")
        print(json.dumps(payload, indent=2))
        return 0 if payload["status"] == ReceiptStatus.PASS.value else 2
    if args.reconcile_graph_evidence is not None:
        payload = reconcile_graph_evidence(
            output_dir,
            args.reconcile_graph_evidence,
        )
        payload["receipt"] = str(output_dir / "receipt.json")
        print(json.dumps(payload, indent=2))
        return 0 if payload["status"] == ReceiptStatus.PASS.value else 2
    if args.reconcile_clerk:
        payload = reconcile_clerk_evidence(output_dir, root=root)
        payload["receipt"] = str(output_dir / "receipt.json")
        print(json.dumps(payload, indent=2))
        return 0 if payload["status"] == ReceiptStatus.PASS.value else 2
    receipt = build_receipt(
        output_dir=output_dir,
        graph_evidence=args.graph_evidence,
        run_launchers=args.run_launchers,
        run_clerk=args.run_clerk,
        root=root,
    )
    path = output_dir / "receipt.json"
    write_json_atomic(path, receipt.to_dict())
    payload = receipt.to_dict()
    payload["receipt"] = str(path)
    print(json.dumps(payload, indent=2))
    return 0 if receipt.status is ReceiptStatus.PASS else 2


if __name__ == "__main__":
    raise SystemExit(main())
