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
from collections.abc import Iterable
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Final

from conductor import workspace_launcher_smokes as _launcher_smokes
from conductor import workspace_runtime_support as _runtime_support
from conductor._native import workspace_runtime_matrix_native
from conductor.active_state import save_active_state
from conductor.audit_root import (
    AuditRootError,
    print_audit_provenance,
    resolve_audit_root,
)
from conductor.candidate_review.model import write_json_atomic
from conductor.candidate_review.ownership import load_claims
from conductor.http_transport import open_http
from conductor.local_ai_policy import CLERK_SYSTEM_PROMPT
from conductor.project_paths import host_root
from conductor.workspace_runtime_types import CellReceipt, LauncherSpec, ReceiptStatus

ROOT: Final[Path] = host_root()
# Relative to --root (see main()), not to Path(__file__) -- a caller in a
# different worktree must not write receipts into some other checkout.
DEFAULT_OUTPUT: Final[Path] = Path("research/reports/workspace_reliability_20260823")
EMBED_MODEL: Final[str] = "qwen3-embed-cpu"
CLERK_MODEL: Final[str] = "qwen3.5:9b"
CLERK_NUM_CTX: Final[int] = 2048
CLERK_NUM_GPU: Final[int] = 99
CLERK_NUM_PREDICT: Final[int] = 32
PROHIBITED_MODEL_FRAGMENTS: Final[tuple[str, ...]] = ("qwen3.8", "27b")
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


def _matrix_native(operation: str, payload: Any) -> Any:
    """Cross the native policy boundary with JSON-shaped captured evidence."""
    return json.loads(workspace_runtime_matrix_native(operation, json.dumps(payload)))


def _cell_native(operation: str, payload: Any) -> CellReceipt:
    result = _matrix_native(operation, payload)
    result["status"] = ReceiptStatus(result["status"])
    return CellReceipt(**result)


def _sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _sha256_path(path: Path) -> str:
    return _sha256_bytes(path.read_bytes())


def aggregate_status(cells: Iterable[CellReceipt]) -> ReceiptStatus:
    return ReceiptStatus(
        _matrix_native(
            "aggregate_status",
            [
                {"status": cell.status.value, "required": cell.required}
                for cell in cells
            ],
        )
    )


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
    now = datetime.now(UTC)
    live_ids = sorted(claim.claim_id for claim in claims if claim.active(now))
    cached_ids = sorted(str(claim.get("claim_id")) for claim in state.active_claims)
    age = (now - datetime.fromisoformat(state.last_updated)).total_seconds()
    return _cell_native(
        "check_active_state",
        {
            "live_ids": live_ids,
            "cached_ids": cached_ids,
            "age_seconds": age,
            "active_state_sha256": _sha256_path(state_path),
            "claim_store_sha256": digest,
        },
    )


def _load_json(path: Path) -> dict[str, Any]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError(f"expected JSON object: {path}")  # noqa: TRY004 - public API
    return payload


def check_hook_configs(repo: Path = ROOT) -> CellReceipt:
    return _cell_native("check_hook_configs", {"root": str(repo)})


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
    results = []
    program = root / ".codex" / "hooks" / "pre-edit.sh"
    for case in _matrix_native("hook_program_cases", {}):
        result = _hook_call(case["payload"], program)
        results.append(
            {
                "name": case["name"],
                "expect_deny": case["expect_deny"],
                "returncode": result.returncode,
                "stdout": result.stdout,
            }
        )
    verdict = _matrix_native("hook_program_verdict", results)
    failures: list[str] = verdict["failures"]
    evidence: dict[str, Any] = verdict["evidence"]
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
    return _cell_native("check_launchers", {"missing": missing, "versions": versions})


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
        raise ValueError(f"expected JSON object from {url}")  # noqa: TRY004 - public API
    return result


def _ollama_ps() -> str:
    result = _run(["ollama", "ps"], timeout=15)
    return (result.stdout + result.stderr).strip()


def _ollama_model_rows(raw: str) -> tuple[str, ...]:
    try:
        return tuple(_matrix_native("ollama_model_rows", raw))
    except ValueError as exc:
        raise RuntimeError(str(exc)) from exc


def _nonnegative_int(payload: dict[str, Any], key: str) -> int:
    return _matrix_native("nonnegative_int", {"payload": payload, "key": key})


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
    try:
        return tuple(
            GpuComputeProcess(**process)
            for process in _matrix_native(
                "parse_gpu_processes", {"stdout": result.stdout}
            )
        )
    except ValueError as exc:
        raise RuntimeError(str(exc)) from exc


def clerk_gpu_preflight(repo: Path = ROOT) -> ClerkGpuPreflight:
    """Refuse a 9B load while novel research may own the accelerator."""

    now = datetime.now(UTC)
    claims, _digest = load_claims(repo)
    active_claims = [
        {
            "claim_id": claim.claim_id,
            "owner": str(claim.owner),
            "justification": str(claim.justification),
            "paths": [str(path) for path in claim.paths],
        }
        for claim in claims
        if claim.active(now)
    ]
    result = _matrix_native(
        "gpu_preflight",
        {
            "claims": active_claims,
            "processes": [asdict(process) for process in _gpu_compute_processes()],
            "ollama_ps": _ollama_ps(),
        },
    )
    return ClerkGpuPreflight(
        ready=result["ready"],
        blocking_claim_ids=tuple(result["blocking_claim_ids"]),
        blocking_processes=tuple(
            GpuComputeProcess(**row) for row in result["blocking_processes"]
        ),
        loaded_models=tuple(result["loaded_models"]),
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
    return _cell_native(
        "check_embedding",
        {
            "health": health,
            "processes": _ollama_ps(),
            "dimension": len(vector) if isinstance(vector, list) else None,
            "finite": finite,
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
    results: dict[str, Any] = {}
    for name, argv in commands.items():
        try:
            result = _run(argv, timeout=120)
        except subprocess.TimeoutExpired:
            results[name] = {"timeout": True}
            continue
        results[name] = {"returncode": result.returncode, "stdout": result.stdout}
    return _cell_native("check_retrievers", results)


def extract_reported_tokens(output: str) -> int:
    return _matrix_native("extract_reported_tokens", output)


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
    replacements = [
        check_hook_configs(repo),
        check_hook_programs(repo),
        reconcile_launcher_logs(output_dir),
    ]
    payload = _matrix_native(
        "replace_receipt_cells",
        {
            "receipt": payload,
            "require_launcher": True,
            "replacements": {
                cell.cell_id: {**asdict(cell), "status": cell.status.value}
                for cell in replacements
            },
        },
    )
    provenance = payload["provenance"]
    provenance.update(
        {
            "launcher_usage_reconciled_at": datetime.now(UTC).isoformat(),
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
    payload = _matrix_native(
        "replace_receipt_cells",
        {
            "receipt": payload,
            "single_cell": replacement.cell_id,
            "replacements": {
                replacement.cell_id: {
                    **asdict(replacement),
                    "status": replacement.status.value,
                }
            },
        },
    )
    provenance = payload["provenance"]
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
            "graph_evidence_reconciled_at": datetime.now(UTC).isoformat(),
            "graph_evidence": str(graph_evidence),
            "graph_evidence_sha256": _sha256_path(graph_evidence),
        },
    )


def _clerk_schema() -> dict[str, Any]:
    return _matrix_native("clerk_schema", {})


def _clerk_payload(schema: dict[str, Any]) -> dict[str, Any]:
    schema_text = json.dumps(schema, sort_keys=True, separators=(",", ":"))
    return _matrix_native(
        "clerk_payload",
        {
            "schema": schema,
            "schema_text": schema_text,
            "system_prompt": CLERK_SYSTEM_PROMPT,
        },
    )


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
    return _matrix_native(
        "clerk_unavailable",
        {
            "preflight": preflight.to_evidence(),
            "after_processes": attempt.after_processes,
            "request_error": attempt.request_error,
            "stop_returncode": attempt.stop_returncode,
        },
    )


def _adjudicate_clerk_attempt(
    preflight: ClerkGpuPreflight,
    attempt: ClerkAttempt,
) -> tuple[bool, dict[str, Any]]:
    if attempt.response is None:
        raise ValueError("cannot adjudicate a missing clerk response")
    result = _matrix_native(
        "clerk_adjudicate",
        {
            "preflight": preflight.to_evidence(),
            "response": attempt.response,
            "resident_processes": attempt.resident_processes,
            "after_processes": attempt.after_processes,
            "stop_returncode": attempt.stop_returncode,
            "response_sha256": _sha256_bytes(
                json.dumps(attempt.response, sort_keys=True).encode()
            ),
        },
    )
    return result["ok"], result["evidence"]


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
            "clerk_evidence_reconciled_at": datetime.now(UTC).isoformat(),
            "clerk_evidence": str(output_dir / "local_clerk.json"),
        },
    )


def load_graph_evidence(path: Path | None) -> CellReceipt:
    return _cell_native(
        "check_graph_evidence",
        {
            "path": None if path is None else str(path),
        },
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
        generated_at=datetime.now(UTC).isoformat(),
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
