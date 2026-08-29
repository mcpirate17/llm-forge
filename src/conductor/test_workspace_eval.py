from __future__ import annotations

import json
from pathlib import Path

from conductor import workspace_eval as wseval
from conductor import workspace_runtime_matrix as matrix


def test_offline_eval_is_honestly_not_ready() -> None:
    result = wseval.evaluate(live=False)
    assert result.status == matrix.ReceiptStatus.NOT_READY.value
    assert result.is_valid is False
    assert result.score < 100
    assert any("embedding-canary" in item for item in result.diagnostics)


def test_complete_runtime_receipt_is_required_for_pass(tmp_path: Path) -> None:
    path = tmp_path / "receipt.json"
    cells = [
        {
            "cell_id": cell_id,
            "status": matrix.ReceiptStatus.PASS.value,
            "detail": "fixture",
            "required": True,
            "evidence": {},
        }
        for cell_id in sorted(wseval.REQUIRED_CELL_IDS)
    ]
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "status": matrix.ReceiptStatus.PASS.value,
                "cells": cells,
                "provenance": {"fixture": True},
            }
        ),
        encoding="utf-8",
    )

    result = wseval.evaluate(live=False, runtime_receipt=path)
    assert result.status == matrix.ReceiptStatus.PASS.value
    assert result.is_valid is True
    assert result.score == 100.0


def test_missing_or_nonpass_runtime_cell_fails_closed(tmp_path: Path) -> None:
    path = tmp_path / "receipt.json"
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "status": matrix.ReceiptStatus.PASS.value,
                "cells": [
                    {
                        "cell_id": "active-state-live-claims",
                        "status": matrix.ReceiptStatus.NOT_READY.value,
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    result = wseval.evaluate(runtime_receipt=path)
    assert result.status == matrix.ReceiptStatus.FAIL_CLOSED.value
    assert result.is_valid is False


def test_write_receipt_preserves_status(tmp_path: Path) -> None:
    result = wseval.evaluate(live=False)
    path = wseval.write_receipt(result, receipts_dir=tmp_path)
    payload = json.loads(path.read_text(encoding="utf-8"))
    assert payload["status"] == result.status
    assert payload["metrics"]["provenance"]["oracle"].endswith(
        "workspace_runtime_matrix"
    )
