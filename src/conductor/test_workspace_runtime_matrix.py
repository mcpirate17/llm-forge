from __future__ import annotations

import json
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

from conductor import workspace_runtime_matrix as matrix


def _cell(status: matrix.ReceiptStatus) -> matrix.CellReceipt:
    return matrix.CellReceipt("cell", status, "test")


def test_aggregate_status_precedence() -> None:
    assert (
        matrix.aggregate_status([_cell(matrix.ReceiptStatus.PASS)])
        is matrix.ReceiptStatus.PASS
    )
    assert (
        matrix.aggregate_status(
            [_cell(matrix.ReceiptStatus.PASS), _cell(matrix.ReceiptStatus.NOT_READY)]
        )
        is matrix.ReceiptStatus.NOT_READY
    )
    assert (
        matrix.aggregate_status(
            [
                _cell(matrix.ReceiptStatus.NOT_READY),
                _cell(matrix.ReceiptStatus.FAIL_CLOSED),
            ]
        )
        is matrix.ReceiptStatus.FAIL_CLOSED
    )


def test_optional_cell_does_not_block_pass() -> None:
    optional = matrix.CellReceipt(
        "optional", matrix.ReceiptStatus.FAIL_CLOSED, "ignored", required=False
    )
    assert (
        matrix.aggregate_status([_cell(matrix.ReceiptStatus.PASS), optional])
        is matrix.ReceiptStatus.PASS
    )


def test_graph_evidence_requires_semantic_result(tmp_path: Path) -> None:
    path = tmp_path / "graph.json"
    path.write_text(
        json.dumps(
            {
                "provider": "workspace:provider-fingerprint",
                "backend_fingerprint": "sha256:backend-fingerprint",
                "stored_provider": "workspace:provider-fingerprint",
                "model": "local-or-paid-model",
                "dimension": 1024,
                "paid": False,
                "search_mode": "keyword",
                "result_count": 0,
                "node_count": 39013,
                "live_non_file_node_count": 35000,
                "embedded_node_count": 35000,
                "missing_embedding_count": 0,
                "mixed_provider_live_count": 0,
                "orphan_embedding_count": 0,
                "expected_result_found": True,
                "query_trace": {
                    "provider_name": "workspace:provider-fingerprint",
                    "backend_fingerprint": "sha256:backend-fingerprint",
                    "purpose": "query",
                    "vector_count": 1,
                    "broker_calls": 1,
                    "dimension": 1024,
                    "paid": False,
                },
            }
        ),
        encoding="utf-8",
    )
    assert matrix.load_graph_evidence(path).status is matrix.ReceiptStatus.FAIL_CLOSED
    path.write_text(
        json.dumps(
            {
                "provider": "workspace:provider-fingerprint",
                "backend_fingerprint": "sha256:backend-fingerprint",
                "stored_provider": "workspace:provider-fingerprint",
                "model": "local-or-paid-model",
                "dimension": 1536,
                "paid": True,
                "search_mode": "hybrid",
                "result_count": 2,
                "node_count": 39013,
                "live_non_file_node_count": 35000,
                "embedded_node_count": 35000,
                "missing_embedding_count": 0,
                "mixed_provider_live_count": 0,
                "orphan_embedding_count": 0,
                "expected_result_found": True,
                "query_trace": {
                    "provider_name": "workspace:provider-fingerprint",
                    "backend_fingerprint": "sha256:backend-fingerprint",
                    "purpose": "query",
                    "vector_count": 1,
                    "broker_calls": 1,
                    "dimension": 1536,
                    "paid": True,
                },
            }
        ),
        encoding="utf-8",
    )
    assert matrix.load_graph_evidence(path).status is matrix.ReceiptStatus.PASS


def test_graph_evidence_rejects_mixed_provider_rows(tmp_path: Path) -> None:
    path = tmp_path / "graph.json"
    path.write_text(
        json.dumps(
            {
                "provider": "workspace:provider-fingerprint",
                "backend_fingerprint": "sha256:backend-fingerprint",
                "stored_provider": "workspace:provider-fingerprint",
                "model": "model",
                "dimension": 1024,
                "paid": False,
                "search_mode": "hybrid",
                "result_count": 1,
                "node_count": 2,
                "live_non_file_node_count": 1,
                "embedded_node_count": 1,
                "missing_embedding_count": 0,
                "mixed_provider_live_count": 1,
                "orphan_embedding_count": 0,
                "expected_result_found": True,
                "query_trace": {
                    "provider_name": "workspace:provider-fingerprint",
                    "backend_fingerprint": "sha256:backend-fingerprint",
                    "purpose": "query",
                    "vector_count": 1,
                    "broker_calls": 1,
                    "dimension": 1024,
                    "paid": False,
                },
            }
        ),
        encoding="utf-8",
    )

    assert matrix.load_graph_evidence(path).status is matrix.ReceiptStatus.FAIL_CLOSED


def test_missing_graph_evidence_is_not_ready(tmp_path: Path) -> None:
    assert (
        matrix.load_graph_evidence(tmp_path / "missing.json").status
        is matrix.ReceiptStatus.NOT_READY
    )


def test_extract_reported_tokens_from_jsonl() -> None:
    output = "\n".join(
        [
            json.dumps(
                {
                    "usage": {
                        "input_tokens": 11,
                        "cached_input_tokens": 9,
                        "output_tokens": 7,
                    }
                }
            ),
            "plain text",
            json.dumps(
                {
                    "type": "result",
                    "usage": {
                        "input_tokens": 21,
                        "output_tokens": 4,
                        "total_tokens": 25,
                    },
                }
            ),
        ]
    )
    assert matrix.extract_reported_tokens(output) == 25


def test_hook_program_controls_pass(hook_repo: Path) -> None:
    cell = matrix.check_hook_programs(hook_repo)
    assert cell.status is matrix.ReceiptStatus.PASS, cell.detail


def test_grok_inspect_command_is_injectable(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("GROK_INSPECT_COMMAND", raising=False)
    assert matrix._grok_inspect_argv() == ["grok", "inspect", "--json"]

    monkeypatch.setenv("GROK_INSPECT_COMMAND", "python -m conductor.grok_inspect_stub")
    assert matrix._grok_inspect_argv() == [
        "python",
        "-m",
        "conductor.grok_inspect_stub",
    ]


def test_launcher_specs_cover_required_programs() -> None:
    specs = matrix.launcher_specs()

    assert tuple(spec.name for spec in specs) == matrix.REQUIRED_LAUNCHERS
    assert len(specs) == matrix.MAX_LAUNCHER_CALLS
    codex = next(spec for spec in specs if spec.name == "codex")
    assert "--ask-for-approval" not in codex.argv
    assert "--dangerously-bypass-hook-trust" in codex.argv
    for name in ("claude", "glm"):
        assert "--verbose" in next(spec for spec in specs if spec.name == name).argv


def test_reconcile_receipt_uses_preserved_terminal_usage(
    tmp_path: Path, hook_repo: Path
) -> None:
    launchers = tmp_path / "launchers"
    launchers.mkdir()
    for index, name in enumerate(matrix.REQUIRED_LAUNCHERS, start=1):
        (launchers / f"{name}.log").write_text(
            json.dumps(
                {
                    "type": "result",
                    "usage": {
                        "input_tokens": index * 10,
                        "cached_input_tokens": index * 7,
                        "output_tokens": index,
                    },
                }
            ),
            encoding="utf-8",
        )
        (launchers / f"{name}.json").write_text(
            json.dumps({"launcher": name, "status": "PASS"}), encoding="utf-8"
        )
    (tmp_path / "receipt.json").write_text(
        json.dumps(
            {
                "status": "FAIL-CLOSED",
                "cells": [
                    {
                        "cell_id": "launcher-real-smokes",
                        "status": "FAIL-CLOSED",
                        "detail": "bad parser",
                        "required": True,
                        "evidence": {},
                    }
                ],
                "provenance": {},
            }
        ),
        encoding="utf-8",
    )

    payload = matrix.reconcile_receipt(tmp_path, repo=hook_repo)

    assert payload["status"] == "PASS"
    assert payload["cells"][0]["evidence"]["reported_tokens"] == 165
    assert (tmp_path / "receipt.pre_reconcile.json").is_file()


def test_reconcile_graph_evidence_preserves_other_cells(tmp_path: Path) -> None:
    graph_path = tmp_path / "graph.json"
    graph_path.write_text(
        json.dumps(
            {
                "provider": "workspace:provider-fingerprint",
                "backend_fingerprint": "sha256:backend-fingerprint",
                "stored_provider": "workspace:provider-fingerprint",
                "model": "paid-model",
                "dimension": 1536,
                "paid": True,
                "search_mode": "hybrid",
                "result_count": 1,
                "node_count": 2,
                "live_non_file_node_count": 1,
                "embedded_node_count": 1,
                "missing_embedding_count": 0,
                "mixed_provider_live_count": 0,
                "orphan_embedding_count": 0,
                "expected_result_found": True,
                "query_trace": {
                    "provider_name": "workspace:provider-fingerprint",
                    "backend_fingerprint": "sha256:backend-fingerprint",
                    "purpose": "query",
                    "vector_count": 1,
                    "broker_calls": 1,
                    "dimension": 1536,
                    "paid": True,
                },
            }
        ),
        encoding="utf-8",
    )
    (tmp_path / "receipt.json").write_text(
        json.dumps(
            {
                "status": "FAIL-CLOSED",
                "cells": [
                    {
                        "cell_id": "graph-semantic-runtime",
                        "status": "FAIL-CLOSED",
                        "detail": "old failure",
                        "required": True,
                        "evidence": {},
                    },
                    {
                        "cell_id": "expensive-cell",
                        "status": "PASS",
                        "detail": "preserve me",
                        "required": True,
                        "evidence": {"tokens": 99},
                    },
                ],
                "provenance": {},
            }
        ),
        encoding="utf-8",
    )

    payload = matrix.reconcile_graph_evidence(tmp_path, graph_path)

    assert payload["status"] == "PASS"
    assert payload["cells"][0]["status"] == "PASS"
    assert payload["cells"][1]["evidence"] == {"tokens": 99}
    assert (tmp_path / "receipt.pre_graph_reconcile.json").is_file()


def test_clerk_gpu_preflight_blocks_active_novel_gpu_claim(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    claims = (
        SimpleNamespace(
            claim_id="claim-nm-f6",
            owner="codex-nm-f6-head",
            justification="AVO throughput candidate",
            paths=("research/tools/nm_f6_candidate.py",),
            active=lambda _now: True,
        ),
        SimpleNamespace(
            claim_id="claim-cuda-path",
            owner="codex",
            justification="partitioned output head implementation",
            paths=("research/tools/_nm_f6_partitioned_head_cuda.cu",),
            active=lambda _now: True,
        ),
    )
    monkeypatch.setattr(matrix, "load_claims", lambda _repo: (claims, "digest"))
    monkeypatch.setattr(matrix, "_gpu_compute_processes", lambda: ())
    monkeypatch.setattr(matrix, "_ollama_ps", lambda: "NAME ID SIZE PROCESSOR")

    preflight = matrix.clerk_gpu_preflight()

    assert preflight.ready is False
    assert preflight.blocking_claim_ids == ("claim-cuda-path", "claim-nm-f6")


def test_clerk_gpu_preflight_blocks_compute_process_and_loaded_model(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(matrix, "load_claims", lambda _repo: ((), "digest"))
    monkeypatch.setattr(
        matrix,
        "_gpu_compute_processes",
        lambda: (
            matrix.GpuComputeProcess(42, "python", 4096),
            matrix.GpuComputeProcess(43, "gnome-remote-desktop", 512),
        ),
    )
    monkeypatch.setattr(
        matrix,
        "_ollama_ps",
        lambda: "NAME ID SIZE PROCESSOR\nother-model id 1GB 100% GPU",
    )

    preflight = matrix.clerk_gpu_preflight()

    assert preflight.ready is False
    assert [process.pid for process in preflight.blocking_processes] == [42]
    assert preflight.loaded_models == ("other-model id 1GB 100% GPU",)


def test_ollama_model_rows_and_token_metrics_fail_closed() -> None:
    assert matrix._ollama_model_rows("NAME ID SIZE PROCESSOR") == ()
    assert matrix._ollama_model_rows(
        "NAME ID SIZE PROCESSOR\nqwen3.5:9b id 6.6GB 100% GPU"
    ) == ("qwen3.5:9b id 6.6GB 100% GPU",)
    assert matrix._nonnegative_int({"tokens": 4}, "tokens") == 4
    assert matrix._nonnegative_int({"tokens": None}, "tokens") == -1
    with pytest.raises(RuntimeError, match="unexpected ollama ps"):
        matrix._ollama_model_rows("connection failed")


def _ready_clerk_preflight() -> matrix.ClerkGpuPreflight:
    return matrix.ClerkGpuPreflight(True, (), (), ())


def test_clerk_canary_defers_without_loading_during_avo(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    preflight = matrix.ClerkGpuPreflight(
        False,
        ("claim-nm-f6",),
        (),
        (),
    )
    monkeypatch.setattr(matrix, "clerk_gpu_preflight", lambda: preflight)
    monkeypatch.setattr(
        matrix,
        "_http_json",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(
            AssertionError("9B model must not load")
        ),
    )

    cell = matrix.run_clerk_canary(tmp_path)

    assert cell.status is matrix.ReceiptStatus.NOT_READY
    assert cell.evidence["preflight"]["blocking_claim_ids"] == ["claim-nm-f6"]


def test_clerk_canary_passes_bounded_schema_and_unloads(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    request: dict[str, object] = {}
    monkeypatch.setattr(matrix, "clerk_gpu_preflight", _ready_clerk_preflight)

    def fake_http(_url: str, *, payload: dict, timeout: float) -> dict:
        request.update({"payload": payload, "timeout": timeout})
        return {
            "model": matrix.CLERK_MODEL,
            "message": {
                "content": '{"status":"PASS","cells":5}',
                "thinking": "",
            },
            "done": True,
            "done_reason": "stop",
            "prompt_eval_count": 20,
            "eval_count": 9,
        }

    ps_outputs = iter(
        [
            f"NAME ID SIZE PROCESSOR\n{matrix.CLERK_MODEL} id 6.6GB 100% GPU",
            "NAME ID SIZE PROCESSOR",
        ]
    )
    monkeypatch.setattr(matrix, "_http_json", fake_http)
    monkeypatch.setattr(matrix, "_ollama_ps", lambda: next(ps_outputs))
    monkeypatch.setattr(
        matrix,
        "_run",
        lambda *_args, **_kwargs: subprocess.CompletedProcess([], 0, "", ""),
    )

    cell = matrix.run_clerk_canary(tmp_path)

    assert cell.status is matrix.ReceiptStatus.PASS
    payload = request["payload"]
    assert payload["think"] is False
    assert payload["options"] == {
        "num_ctx": 2048,
        "num_gpu": 99,
        "num_predict": 32,
        "presence_penalty": 0,
        "seed": 0,
        "temperature": 0,
    }
    assert "zero authority" in payload["messages"][0]["content"]
    assert '"additionalProperties":false' in payload["messages"][1]["content"]
    evidence = json.loads((tmp_path / "local_clerk.json").read_text())
    assert evidence["schema_valid"] is True
    assert evidence["unloaded"] is True


def test_clerk_canary_invalid_schema_fails_closed_and_unloads(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setattr(matrix, "clerk_gpu_preflight", _ready_clerk_preflight)
    monkeypatch.setattr(
        matrix,
        "_http_json",
        lambda *_args, **_kwargs: {
            "model": matrix.CLERK_MODEL,
            "message": {"content": "not-json", "thinking": ""},
            "done": True,
            "done_reason": "stop",
            "prompt_eval_count": 20,
            "eval_count": 32,
        },
    )
    ps_outputs = iter(
        [
            f"NAME ID SIZE PROCESSOR\n{matrix.CLERK_MODEL} id 6.6GB 100% GPU",
            "NAME ID SIZE PROCESSOR",
        ]
    )
    monkeypatch.setattr(matrix, "_ollama_ps", lambda: next(ps_outputs))
    monkeypatch.setattr(
        matrix,
        "_run",
        lambda *_args, **_kwargs: subprocess.CompletedProcess([], 0, "", ""),
    )

    cell = matrix.run_clerk_canary(tmp_path)

    assert cell.status is matrix.ReceiptStatus.FAIL_CLOSED
    assert cell.evidence["schema_valid"] is False
    assert cell.evidence["unloaded"] is True


def test_reconcile_clerk_preserves_expensive_cells(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    (tmp_path / "receipt.json").write_text(
        json.dumps(
            {
                "status": "FAIL-CLOSED",
                "cells": [
                    {
                        "cell_id": "local-clerk-canary",
                        "status": "FAIL-CLOSED",
                        "detail": "old failure",
                        "required": True,
                        "evidence": {},
                    },
                    {
                        "cell_id": "expensive-launchers",
                        "status": "PASS",
                        "detail": "preserve me",
                        "required": True,
                        "evidence": {"tokens": 99},
                    },
                ],
                "provenance": {},
            }
        ),
        encoding="utf-8",
    )
    monkeypatch.setattr(
        matrix,
        "run_clerk_canary",
        lambda _output: matrix.CellReceipt(
            "local-clerk-canary",
            matrix.ReceiptStatus.PASS,
            "fixed",
            evidence={"schema_valid": True},
        ),
    )

    payload = matrix.reconcile_clerk_evidence(tmp_path)

    assert payload["status"] == "PASS"
    assert payload["cells"][0]["status"] == "PASS"
    assert payload["cells"][1]["evidence"] == {"tokens": 99}
    assert (tmp_path / "receipt.pre_clerk_reconcile.json").is_file()
