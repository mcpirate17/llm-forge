"""Compatibility seam for the native A2A retention command.

The eligibility, evidence, transaction, and receipt contracts live in
native/forge/tests/mailbox_retention.rs.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from conductor import a2a_retention as retention
from conductor.a2a_registry import A2aError


def _native_response(store: Path) -> str:
    return json.dumps(
        {
            "schema_version": 1,
            "authority": "deterministic-a2a-retention",
            "automatic": False,
            "mode": "preview",
            "results": [
                {
                    "store": str(store),
                    "mode": "preview",
                    "eligible": 1,
                    "compacted": 0,
                    "original_content_bytes": 21,
                    "tombstone_bytes": 53,
                    "logical_bytes_removed": 0,
                    "evidence_files": 0,
                    "evidence_protected": 0,
                    "evidence_snapshot_sha256": "a" * 64,
                    "manifest_sha256": ["b" * 64],
                    "event_sha256": ["c" * 64],
                }
            ],
        }
    )


def test_public_api_forwards_exact_scope_and_time_to_native(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    store = tmp_path / "a2a" / "worker" / "store.sqlite"
    evidence = tmp_path / "evidence"
    now = datetime(2026, 8, 30, 12, tzinfo=UTC)
    calls: list[list[str]] = []
    monkeypatch.setattr(retention, "_forge_binary", lambda: Path("/fake/forge"))

    def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(command)
        assert kwargs == {"capture_output": True, "text": True, "check": False}
        return subprocess.CompletedProcess(command, 0, _native_response(store), "")

    monkeypatch.setattr(retention.subprocess, "run", run)
    result = retention.compact_resolved_messages(
        store,
        actor="worker",
        evidence_root=evidence,
        now=now,
        grace=timedelta(hours=3),
        limit=7,
        apply=True,
    )
    assert result.store == str(store)
    assert result.manifest_sha256 == ["b" * 64]
    assert calls == [
        [
            "/fake/forge",
            "mailbox",
            "--state-dir",
            str(store.parent.parent),
            "retention",
            "--evidence-root",
            str(evidence),
            "--grace-hours",
            "3.0",
            "--limit",
            "7",
            "--store",
            "worker",
            "--as-name",
            "worker",
            "--now",
            "2026-08-30T12:00:00+00:00",
            "--apply",
        ]
    ]


def test_bridge_fails_closed_on_invalid_time_and_native_receipt(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(retention, "_forge_binary", lambda: Path("/fake/forge"))
    called = False

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        nonlocal called
        called = True
        return subprocess.CompletedProcess(
            command, 0, '{"authority":"wrong","results":[]}', ""
        )

    monkeypatch.setattr(retention.subprocess, "run", run)
    with pytest.raises(A2aError, match="timezone-aware"):
        retention.sweep(tmp_path, now=datetime(2026, 8, 30, 12))  # noqa: DTZ001 -- rejection fixture
    assert not called
    with pytest.raises(A2aError, match="retention grace"):
        retention.sweep(tmp_path, grace=timedelta(minutes=59))
    assert not called
    with pytest.raises(A2aError, match="wrong native retention authority"):
        retention.sweep(tmp_path)
    assert called


def test_cli_is_preview_by_default_and_preserves_native_errors(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    store = tmp_path / "worker" / "store.sqlite"
    monkeypatch.setattr(retention, "_forge_binary", lambda: Path("/fake/forge"))

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        assert "--apply" not in command
        return subprocess.CompletedProcess(command, 0, _native_response(store), "")

    monkeypatch.setattr(retention.subprocess, "run", run)
    assert retention.main(["--state-dir", str(tmp_path), "--store", "worker"]) == 0
    assert json.loads(capsys.readouterr().out)["mode"] == "preview"

    def fail(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(command, 2, "", "exact store required")

    monkeypatch.setattr(retention.subprocess, "run", fail)
    assert retention.main(["--state-dir", str(tmp_path), "--apply"]) == 2
    assert "exact store required" in capsys.readouterr().err


def test_import_has_no_store_or_scheduler_side_effect(tmp_path: Path) -> None:
    package_root = Path(retention.__file__).resolve().parents[1]
    state_dir = tmp_path / "must-not-exist"
    script = (
        "import sqlite3; "
        "sqlite3.connect=lambda *a, **k: (_ for _ in ()).throw("
        "AssertionError('import opened sqlite')); "
        "import conductor.a2a_retention"
    )
    completed = subprocess.run(
        [sys.executable, "-c", script],
        cwd=tmp_path,
        env={
            **os.environ,
            "PYTHONPATH": str(package_root),
            "PYTHONDONTWRITEBYTECODE": "1",
            "A2A_STATE_DIR": str(state_dir),
        },
        check=False,
        capture_output=True,
        text=True,
    )
    assert completed.returncode == 0, completed.stderr
    assert not state_dir.exists()
    agent_source = (package_root / "conductor" / "agent_a2a.py").read_text()
    assert "a2a_retention" not in agent_source
    assert "compact_resolved_messages" not in agent_source
