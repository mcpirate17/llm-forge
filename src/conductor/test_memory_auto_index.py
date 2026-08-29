from __future__ import annotations

import json
import subprocess
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

from conductor import memory_auto_index as auto


def _catalog(path: Path, notes: Path) -> Path:
    path.write_text(
        "\n".join(
            [
                "schema_version = 1",
                "[[source]]",
                'id = "notes"',
                'kind = "index"',
                f"absolute_root = {json.dumps(str(notes))}",
                'glob = "*.md"',
                'chunk = "heading"',
            ]
        ),
        encoding="utf-8",
    )
    return path


def _payload(path: Path) -> dict[str, object]:
    return {
        "hook_event_name": "PostToolUse",
        "tool_name": "write_file",
        "cwd": str(path.parent),
        "tool_input": {"file_path": str(path)},
    }


def _success(updated: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.CompletedProcess(
        args=["memory-index"],
        returncode=0,
        stdout=json.dumps(
            {
                "index": "/tmp/index.jsonl",
                "chunks": 2,
                "sources": ["notes"],
                "updated": updated,
            }
        )
        + "\n",
        stderr="",
    )


def test_mark_pending_filters_non_indexed_paths(tmp_path: Path) -> None:
    notes = tmp_path / "notes"
    notes.mkdir()
    note = notes / "finding.md"
    other = tmp_path / "code.py"
    note.write_text("finding", encoding="utf-8")
    other.write_text("code", encoding="utf-8")
    catalog = _catalog(tmp_path / "sources.toml", notes)
    state = tmp_path / "state"

    assert (
        auto.mark_pending(
            _payload(other),
            repo_root=tmp_path,
            state_dir=state,
            catalog_path=catalog,
        )
        is None
    )
    marker = auto.mark_pending(
        _payload(note),
        repo_root=tmp_path,
        state_dir=state,
        catalog_path=catalog,
    )
    assert marker is not None and marker.is_file()
    assert len(list((state / "pending").glob("*.json"))) == 1


def test_flush_coalesces_markers_and_writes_receipt(tmp_path: Path) -> None:
    notes = tmp_path / "notes"
    notes.mkdir()
    first = notes / "first.md"
    second = notes / "second.md"
    first.write_text("first", encoding="utf-8")
    second.write_text("second", encoding="utf-8")
    catalog = _catalog(tmp_path / "sources.toml", notes)
    state = tmp_path / "state"
    for note in (first, second):
        auto.mark_pending(
            _payload(note),
            repo_root=tmp_path,
            state_dir=state,
            catalog_path=catalog,
        )
    calls = 0

    def runner(
        _repo: Path, _timeout: float, sources: set[str] | None
    ) -> subprocess.CompletedProcess[str]:
        nonlocal calls
        calls += 1
        assert sources == {"notes"}
        return _success()

    receipts = auto.flush_pending(
        repo_root=tmp_path,
        state_dir=state,
        runner=runner,
    )

    assert calls == 1
    assert receipts[0]["status"] == "PASS"
    assert receipts[0]["pending_markers"] == 2
    assert not list((state / "pending").glob("*.json"))
    assert json.loads((state / "latest.json").read_text())["status"] == "PASS"


def test_flush_single_writer_across_concurrent_calls(tmp_path: Path) -> None:
    state = tmp_path / "state"
    pending = state / "pending"
    pending.mkdir(parents=True)
    (pending / "one.json").write_text('{"paths": ["one.md"]}', encoding="utf-8")
    calls = 0
    calls_lock = threading.Lock()

    def runner(
        _repo: Path, _timeout: float, _sources: set[str] | None
    ) -> subprocess.CompletedProcess[str]:
        nonlocal calls
        with calls_lock:
            calls += 1
        time.sleep(0.05)
        return _success()

    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = [
            pool.submit(
                auto.flush_pending,
                repo_root=tmp_path,
                state_dir=state,
                runner=runner,
            )
            for _ in range(2)
        ]
        results = [future.result() for future in futures]

    assert calls == 1
    assert sorted(len(result) for result in results) == [0, 1]


def test_flush_repeats_when_new_marker_arrives_mid_refresh(tmp_path: Path) -> None:
    notes = tmp_path / "notes"
    notes.mkdir()
    first = notes / "first.md"
    second = notes / "second.md"
    first.write_text("first", encoding="utf-8")
    second.write_text("second", encoding="utf-8")
    catalog = _catalog(tmp_path / "sources.toml", notes)
    state = tmp_path / "state"
    auto.mark_pending(
        _payload(first),
        repo_root=tmp_path,
        state_dir=state,
        catalog_path=catalog,
    )
    calls = 0

    def runner(
        _repo: Path, _timeout: float, sources: set[str] | None
    ) -> subprocess.CompletedProcess[str]:
        nonlocal calls
        calls += 1
        assert sources == {"notes"}
        if calls == 1:
            auto.mark_pending(
                _payload(second),
                repo_root=tmp_path,
                state_dir=state,
                catalog_path=catalog,
            )
        return _success()

    receipts = auto.flush_pending(
        repo_root=tmp_path,
        state_dir=state,
        runner=runner,
    )

    assert calls == 2
    assert len(receipts) == 2
    assert not list((state / "pending").glob("*.json"))


def test_flush_failure_retains_markers_and_fails_closed(tmp_path: Path) -> None:
    state = tmp_path / "state"
    pending = state / "pending"
    pending.mkdir(parents=True)
    marker = pending / "one.json"
    marker.write_text('{"paths": ["one.md"]}', encoding="utf-8")

    def runner(
        _repo: Path, _timeout: float, _sources: set[str] | None
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(
            args=["memory-index"],
            returncode=2,
            stdout="",
            stderr="schema mismatch",
        )

    with pytest.raises(auto.AutoIndexError, match="schema mismatch"):
        auto.flush_pending(
            repo_root=tmp_path,
            state_dir=state,
            runner=runner,
        )

    assert marker.is_file()
    latest = json.loads((state / "latest.json").read_text(encoding="utf-8"))
    assert latest["status"] == "FAIL-CLOSED"
    assert latest["is_valid"] is False


def test_session_end_scan_runs_without_pending_markers(tmp_path: Path) -> None:
    calls = 0

    def runner(
        _repo: Path, _timeout: float, sources: set[str] | None
    ) -> subprocess.CompletedProcess[str]:
        nonlocal calls
        calls += 1
        assert sources is None
        return _success(updated=False)

    receipts = auto.flush_pending(
        repo_root=tmp_path,
        state_dir=tmp_path / "state",
        runner=runner,
        scan_if_clean=True,
    )

    assert calls == 1
    assert receipts[0]["status"] == "PASS"
    assert receipts[0]["pending_markers"] == 0
