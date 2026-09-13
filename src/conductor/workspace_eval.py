#!/usr/bin/env python3
"""Compatibility adapter for the behavior-backed workspace runtime receipt."""

from __future__ import annotations

import argparse
import hashlib
import json
import time
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Final

from conductor import workspace_runtime_matrix as matrix

from conductor.project_paths import host_root
ROOT: Final[Path] = host_root()
RECEIPTS_DIR: Final[Path] = ROOT / "research" / "reports" / "avo_receipts"
REQUIRED_CELL_IDS: Final[frozenset[str]] = frozenset(
    {
        "active-state-live-claims",
        "hook-config-contract",
        "hook-program-controls",
        "launcher-programs",
        "embedding-canary",
        "retriever-runtime",
        "graph-semantic-runtime",
        "launcher-real-smokes",
        "local-clerk-canary",
    }
)


@dataclass(slots=True)
class EvalResult:
    status: str
    is_valid: bool
    score: float
    improved: bool
    elapsed_seconds: float
    mode: str = "workspace"
    metrics: dict[str, Any] = field(default_factory=dict)
    diagnostics: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def _cell_payload(cell: matrix.CellReceipt) -> dict[str, Any]:
    return {
        "status": cell.status.value,
        "detail": cell.detail,
        "required": cell.required,
        "evidence": cell.evidence,
    }


def _load_complete_receipt(
    path: Path,
) -> tuple[matrix.ReceiptStatus, dict[str, Any], list[str]]:
    try:
        raw = path.read_bytes()
        payload = json.loads(raw)
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        return (
            matrix.ReceiptStatus.FAIL_CLOSED,
            {},
            [f"runtime receipt unreadable: {exc}"],
        )
    if not isinstance(payload, dict) or payload.get("schema_version") != 1:
        return (
            matrix.ReceiptStatus.FAIL_CLOSED,
            {},
            ["runtime receipt schema is invalid"],
        )
    rows = payload.get("cells")
    if not isinstance(rows, list):
        return (
            matrix.ReceiptStatus.FAIL_CLOSED,
            {},
            ["runtime receipt cells are missing"],
        )
    cells = {
        str(row.get("cell_id")): row
        for row in rows
        if isinstance(row, dict) and isinstance(row.get("cell_id"), str)
    }
    missing = sorted(REQUIRED_CELL_IDS - cells.keys())
    failing = sorted(
        cell_id
        for cell_id in REQUIRED_CELL_IDS & cells.keys()
        if cells[cell_id].get("status") != matrix.ReceiptStatus.PASS.value
    )
    claimed_status = payload.get("status")
    diagnostics: list[str] = []
    if missing:
        diagnostics.append(f"missing runtime cells: {missing}")
    if failing:
        diagnostics.append(f"non-PASS runtime cells: {failing}")
    if claimed_status != matrix.ReceiptStatus.PASS.value and not diagnostics:
        diagnostics.append(f"runtime receipt status is {claimed_status!r}, not PASS")
    status = (
        matrix.ReceiptStatus.PASS
        if not diagnostics
        else matrix.ReceiptStatus.FAIL_CLOSED
    )
    return (
        status,
        {
            "path": str(path),
            "sha256": hashlib.sha256(raw).hexdigest(),
            "cells": cells,
            "provenance": payload.get("provenance"),
        },
        diagnostics,
    )


def evaluate(
    repo_root: Path = ROOT,
    *,
    live: bool = True,
    seed: int = 0,
    runtime_receipt: Path | None = None,
) -> EvalResult:
    """Return PASS only for a complete behavior-backed runtime receipt."""
    started = time.monotonic()
    if runtime_receipt is not None:
        status, receipt_metrics, diagnostics = _load_complete_receipt(runtime_receipt)
        score = 100.0 if status is matrix.ReceiptStatus.PASS else 0.0
        return EvalResult(
            status=status.value,
            is_valid=status is matrix.ReceiptStatus.PASS,
            score=score,
            improved=False,
            elapsed_seconds=round(time.monotonic() - started, 3),
            metrics={
                "runtime_receipt": receipt_metrics,
                "provenance": {
                    "seed": seed,
                    "config": "conductor.workspace_eval",
                    "oracle": "conductor.workspace_runtime_matrix",
                },
            },
            diagnostics=diagnostics,
        )

    cells = [
        matrix.check_active_state(repo_root),
        matrix.check_hook_configs(repo_root),
        matrix.check_hook_programs(repo_root),
        matrix.check_launcher_programs(),
    ]
    if live:
        cells.extend([matrix.check_embedding_canary(), matrix.check_retrievers()])
    else:
        cells.extend(
            [
                matrix.CellReceipt(
                    "embedding-canary",
                    matrix.ReceiptStatus.NOT_READY,
                    "offline evaluation does not exercise the embedding backend",
                ),
                matrix.CellReceipt(
                    "retriever-runtime",
                    matrix.ReceiptStatus.NOT_READY,
                    "offline evaluation does not exercise retrieval",
                ),
            ]
        )
    cells.extend(
        [
            matrix.CellReceipt(
                "graph-semantic-runtime",
                matrix.ReceiptStatus.NOT_READY,
                "supply a completed runtime receipt with graph evidence",
            ),
            matrix.CellReceipt(
                "launcher-real-smokes",
                matrix.ReceiptStatus.NOT_READY,
                "supply a completed runtime receipt with five launcher calls",
            ),
            matrix.CellReceipt(
                "local-clerk-canary",
                matrix.ReceiptStatus.NOT_READY,
                "supply a completed runtime receipt with the 9B clerk canary",
            ),
        ]
    )
    status = matrix.aggregate_status(cells)
    passed = sum(cell.status is matrix.ReceiptStatus.PASS for cell in cells)
    diagnostics = [
        f"{cell.cell_id}: {cell.detail}"
        for cell in cells
        if cell.status is not matrix.ReceiptStatus.PASS
    ]
    return EvalResult(
        status=status.value,
        is_valid=False,
        score=round(100.0 * passed / len(cells), 3),
        improved=False,
        elapsed_seconds=round(time.monotonic() - started, 3),
        metrics={
            "checks": {cell.cell_id: _cell_payload(cell) for cell in cells},
            "provenance": {
                "seed": seed,
                "config": "conductor.workspace_eval",
                "oracle": "conductor.workspace_runtime_matrix",
            },
        },
        diagnostics=diagnostics,
    )


def write_receipt(result: EvalResult, receipts_dir: Path = RECEIPTS_DIR) -> Path:
    receipts_dir.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    path = receipts_dir / f"workspace_{stamp}.json"
    path.write_text(json.dumps(result.to_dict(), indent=2) + "\n", encoding="utf-8")
    return path


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--runtime-receipt", type=Path)
    parser.add_argument("--write-receipt", action="store_true")
    parser.add_argument("--seed", type=int, default=0)
    args = parser.parse_args(argv)
    result = evaluate(
        live=not args.offline,
        seed=args.seed,
        runtime_receipt=args.runtime_receipt,
    )
    if args.write_receipt:
        result.metrics["receipt"] = str(write_receipt(result))
    print(json.dumps(result.to_dict(), indent=2))
    return 0 if result.is_valid else 2


if __name__ == "__main__":
    raise SystemExit(main())
