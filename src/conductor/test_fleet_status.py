"""Tests for conductor.fleet_status — hermetic; every external source is stubbed."""

from __future__ import annotations

import json
import subprocess
import sys

import pytest

import conductor.fleet_status as fs


def _stub_sources(monkeypatch, peers=None, heard=None, state=None, procs=None):
    monkeypatch.setattr(fs, "read_peers", lambda: dict(peers or {}))
    monkeypatch.setattr(fs, "read_last_heard", lambda: dict(heard or {}))
    monkeypatch.setattr(
        fs,
        "read_state",
        lambda: state or {"active_claims": [], "active_headings": []},
    )
    monkeypatch.setattr(fs, "read_worktree_procs", lambda: dict(procs or {}))


def test_run_raises_on_nonzero():
    argv = [
        sys.executable,
        "-c",
        "import sys; sys.stderr.write('boom boom'); sys.exit(3)",
    ]
    with pytest.raises(fs.FleetStatusError) as exc:
        fs._run(argv)
    msg = str(exc.value)
    assert "exited 3" in msg
    assert "boom boom" in msg


def test_run_returns_stdout():
    out = fs._run([sys.executable, "-c", "print('hello-fleet')"])
    assert out.strip() == "hello-fleet"


def test_read_last_heard_keeps_newest_per_sender(monkeypatch):
    inbox = (
        "[UNREAD] m1 from=seat-a at=2026-09-01T03:00:00+00:00\n"
        "newest words from a\n"
        "\n"
        "[READ] m2 from=seat-a at=2026-09-01T02:00:00+00:00\n"
        "older words from a\n"
        "\n"
        "[READ] m3 from=seat-b at=2026-09-01T01:00:00+00:00\n" + "b" * 200 + "\n"
    )
    monkeypatch.setattr(fs, "_run", lambda argv: inbox)
    heard = fs.read_last_heard()
    assert heard["seat-a"]["at"] == "2026-09-01T03:00:00+00:00"
    assert heard["seat-a"]["said"] == "newest words from a"
    assert heard["seat-b"]["said"] == "b" * 160


def test_heading_seat_extraction():
    heading = "Frozen candidate ready — 2026-09-01 ~02:45 UTC, codex-rust-architecture-learning"
    assert fs._heading_seat(heading) == "codex-rust-architecture-learning"
    assert fs._heading_seat("no trailing seat marker here") == ""


def test_build_report_joins_all_name_sources(monkeypatch):
    _stub_sources(
        monkeypatch,
        peers={"peer-seat": {"status": "up", "port": 7001}},
        heard={"heard-seat": {"at": "2026-09-01T00:00:00", "said": "hi"}},
        state={
            "active_claims": [
                {
                    "owner": "claim-seat",
                    "paths": ["a.py"],
                    "expires_at": "2026-09-02T00:00:00+00:00",
                }
            ],
            "active_headings": ["did a thing — 2026-09-01, heading-seat"],
        },
    )
    report = fs.build_report()
    assert set(report["seats"]) == {
        "peer-seat",
        "heard-seat",
        "claim-seat",
        "heading-seat",
    }


def test_build_report_a2a_status_labels(monkeypatch):
    _stub_sources(
        monkeypatch,
        peers={
            "up-seat": {"status": "up", "port": 7002},
            "down-seat": {"status": "down", "port": 7003},
        },
        heard={"ghost-seat": {"at": "t", "said": "s"}},
    )
    seats = fs.build_report()["seats"]
    assert seats["up-seat"]["a2a"] == "up:7002"
    assert seats["down-seat"]["a2a"] == "down"
    assert seats["ghost-seat"]["a2a"] == "NO IDENTITY"


def test_build_report_claims_aggregation(monkeypatch):
    claims = [
        {
            "owner": "seat-x",
            "paths": ["b.py", "a.py"],
            "expires_at": "2026-09-03T00:00:00+00:00",
        },
        {
            "owner": "seat-x",
            "paths": ["c.py"],
            "expires_at": "2026-09-02T00:00:00+00:00",
        },
    ]
    _stub_sources(monkeypatch, state={"active_claims": claims, "active_headings": []})
    seat = fs.build_report()["seats"]["seat-x"]
    assert seat["claims"] == 2
    assert seat["claim_paths"] == ["a.py", "b.py", "c.py"]
    assert seat["soonest_expiry"] == "2026-09-02T00:00:00+00:00"


def test_render_truncates_claim_paths():
    report = {
        "generated_at": "2026-09-01T00:00:00+00:00",
        "root": "/r",
        "seats": {
            "seat-y": {
                "a2a": "down",
                "last_heard": None,
                "headings": [],
                "claims": 1,
                "claim_paths": ["p1", "p2", "p3", "p4", "p5", "p6"],
                "soonest_expiry": None,
            }
        },
        "worktree_processes": {},
    }
    text = fs.render(report)
    assert "p1, p2, p3, p4 (+2 more)" in text
    assert "p5" not in text


def test_render_worktree_sections():
    report = {
        "generated_at": "t",
        "root": "/r",
        "seats": {},
        "worktree_processes": {
            "/tmp/llm-scratch": ["proc1", "proc2", "proc3", "proc4", "proc5"],
            "/home/tim/Projects/LLM": ["daemon1", "daemon2"],
        },
    }
    text = fs.render(report)
    assert "/tmp/llm-scratch: 5" in text
    assert "proc1" in text
    assert "proc3" in text
    assert "proc4" not in text
    assert "+2 more" in text
    assert "/home/tim/Projects/LLM: 2" in text
    assert "daemon1" not in text


def test_main_json_mode(monkeypatch, capsys):
    report = {"generated_at": "t", "root": "/r", "seats": {}, "worktree_processes": {}}
    monkeypatch.setattr(fs, "build_report", lambda: report)
    assert fs.main(["--json"]) == 0
    assert json.loads(capsys.readouterr().out) == report


def test_main_human_mode(monkeypatch, capsys):
    report = {
        "generated_at": "TSTAMP",
        "root": "/r",
        "seats": {},
        "worktree_processes": {},
    }
    monkeypatch.setattr(fs, "build_report", lambda: report)
    assert fs.main([]) == 0
    assert capsys.readouterr().out.startswith("FLEET STATUS  TSTAMP")


def test_module_entrypoint():
    proc = subprocess.run(
        [sys.executable, "-m", "conductor.fleet_status", "--help"],
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert proc.returncode == 0
    assert "fleet status" in proc.stdout.lower()


def test_worktree_regex_is_configured_via_project_paths():
    # fs._WORKTREE is built from project_paths.worktree_patterns(fs.ROOT) at
    # import time; this pins the observable behaviour without re-importing.
    from conductor import project_paths as pp

    assert fs._WORKTREE.pattern == "(" + "|".join(pp.worktree_patterns(fs.ROOT)) + ")"
    assert fs._WORKTREE.search("/tmp/llm-scratch/foo") is not None
    assert fs._WORKTREE.search("/home/tim/Projects/LLM/bar") is not None
    assert fs._WORKTREE.search("/var/nope") is None
