"""Tests for the PostToolUse Bash output bounding hook."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _bash_quiet as bq  # noqa: E402

HOOK = Path(__file__).resolve().parent / "post-bash-quiet.sh"


@pytest.fixture(autouse=True)
def _save_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setattr(bq, "SAVE_DIR", tmp_path / "out")
    return tmp_path / "out"


def test_bound_passes_small_output_unchanged() -> None:
    data = b"x" * (bq.LIMIT_BYTES - 1)
    assert bq.bound(data) == data


def test_bound_boundary_at_limit_bytes_is_exclusive(_save_dir: Path) -> None:
    at_limit = b"x" * bq.LIMIT_BYTES
    assert bq.bound(at_limit) == at_limit
    over_limit = b"y" * (bq.LIMIT_BYTES + 1)
    assert bq.bound(over_limit) != over_limit
    assert b"[elided" in bq.bound(over_limit)


def test_bound_keeps_head_and_tail_and_saves_full(_save_dir: Path) -> None:
    lines = [f"line {i:05d}\n".encode() for i in range(1000)]
    data = b"".join(lines)
    out = bq.bound(data)
    assert out.startswith(b"line 00000\n") and out.endswith(b"line 00999\n")
    assert b"line 00500" not in out
    assert f"[elided {1000 - bq.HEAD_LINES - bq.TAIL_LINES:,} lines".encode() in out
    saved = list(_save_dir.iterdir())
    assert len(saved) == 1 and saved[0].read_bytes() == data
    assert len(out) < len(data) // 5


def test_bound_cuts_few_long_lines_by_bytes(_save_dir: Path) -> None:
    data = b"a" * 20000 + b"\n" + b"b" * 20000
    out = bq.bound(data)
    assert out.startswith(b"a" * 100) and out.endswith(b"b" * 100)
    assert b"[elided" in out and len(out) < bq.LIMIT_BYTES
    assert len(list(_save_dir.iterdir())) == 1


def test_bound_response_preserves_shape_and_untouched_fields() -> None:
    big = "\n".join(f"row {i}" for i in range(3000))
    response = {
        "stdout": big,
        "stderr": "warn",
        "interrupted": False,
        "exitCode": 0,
        "isImage": False,
    }
    updated = bq.bound_response(response)
    assert updated is not None
    assert set(updated) == set(response)
    assert updated["stderr"] == "warn" and updated["exitCode"] == 0
    assert updated["stdout"].startswith("row 0\n") and updated["stdout"].endswith(
        "row 2999"
    )
    assert "[elided" in updated["stdout"]
    assert bq.bound_response({"stdout": "small", "stderr": ""}) is None
    assert bq.bound_response(None) is None
    assert bq.bound_response(42) is None


def test_bound_response_handles_plain_string_outputs() -> None:
    assert bq.bound_response("short") is None
    long = "\n".join(str(i) for i in range(5000))
    out = bq.bound_response(long)
    assert isinstance(out, str) and "[elided" in out and len(out) < len(long) // 4


def test_hook_output_only_rewrites_when_needed() -> None:
    quiet = bq.hook_output({"tool_response": {"stdout": "ok", "stderr": ""}})
    assert quiet == {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
    assert bq.hook_output("garbage") == quiet
    loud = bq.hook_output({"tool_response": {"stdout": "z\n" * 9000, "stderr": ""}})
    assert "[elided" in loud["hookSpecificOutput"]["updatedToolOutput"]["stdout"]


def test_hook_output_honors_output_field_override(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(bq, "OUTPUT_FIELD", "updatedMCPToolOutput")
    loud = bq.hook_output({"tool_response": {"stdout": "z\n" * 9000, "stderr": ""}})
    assert "updatedToolOutput" not in loud["hookSpecificOutput"]
    assert "[elided" in loud["hookSpecificOutput"]["updatedMCPToolOutput"]["stdout"]


def test_shell_entry_point_respects_codex_output_field_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    payload = {
        "tool_name": "Bash",
        "tool_response": {"stdout": "q\n" * 9000, "stderr": "", "interrupted": False},
    }
    env = {**os.environ, "BASH_QUIET_OUTPUT_FIELD": "updatedMCPToolOutput"}
    proc = subprocess.run(
        ["bash", str(HOOK)],
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        env=env,
        check=True,
    )
    out = json.loads(proc.stdout)["hookSpecificOutput"]
    assert "updatedToolOutput" not in out
    assert out["updatedMCPToolOutput"]["interrupted"] is False


def test_save_dir_env_is_repo_relative_and_falls_back_to_a_tempdir(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("BASH_QUIET_SAVE_DIR", "research/tmp/bash_output")
    assert bq._save_dir() == bq.REPO_ROOT / "research" / "tmp" / "bash_output"
    monkeypatch.setenv("BASH_QUIET_SAVE_DIR", str(tmp_path / "abs"))
    assert bq._save_dir() == tmp_path / "abs"
    monkeypatch.setenv("BASH_QUIET_SAVE_DIR", "  ")
    fallback = bq._save_dir()
    assert not fallback.is_relative_to(bq.REPO_ROOT) and fallback.is_absolute()
    monkeypatch.delenv("BASH_QUIET_SAVE_DIR")
    assert bq._save_dir() == fallback


def test_shell_entry_point_sources_the_project_env_extension(tmp_path: Path) -> None:
    """post-bash-quiet.sh picks up project/env.sh, and works without it."""
    hooks = tmp_path / "repo" / "tooling" / "hooks" / "claude"
    hooks.mkdir(parents=True)
    for name in ("post-bash-quiet.sh", "_bash_quiet.py"):
        shutil.copy2(HOOK.parent / name, hooks / name)
    payload = json.dumps({"tool_response": {"stdout": "q\n" * 9000, "stderr": ""}})
    env = {k: v for k, v in os.environ.items() if k != "BASH_QUIET_SAVE_DIR"}

    def _run() -> str:
        proc = subprocess.run(
            ["bash", str(hooks / "post-bash-quiet.sh")],
            input=payload,
            capture_output=True,
            text=True,
            env=env,
            check=True,
        )
        out = json.loads(proc.stdout)["hookSpecificOutput"]["updatedToolOutput"]
        return out["stdout"]

    assert "[elided" in _run()
    assert not (tmp_path / "repo" / "scratch").exists()

    project = tmp_path / "repo" / ".claude" / "hooks" / "project"
    project.mkdir(parents=True)
    (project / "env.sh").write_text('export BASH_QUIET_SAVE_DIR="scratch/out"\n')
    assert "scratch/out/" in _run()
    saved = list((tmp_path / "repo" / "scratch" / "out").iterdir())
    assert len(saved) == 1 and saved[0].read_text().count("q\n") == 9000


def test_shell_entry_point_round_trips_json() -> None:
    payload = {
        "tool_name": "Bash",
        "tool_response": {"stdout": "q\n" * 9000, "stderr": "", "interrupted": False},
    }
    proc = subprocess.run(
        ["bash", str(HOOK)],
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        check=True,
    )
    out = json.loads(proc.stdout)["hookSpecificOutput"]
    assert out["hookEventName"] == "PostToolUse"
    assert out["updatedToolOutput"]["interrupted"] is False
    assert len(out["updatedToolOutput"]["stdout"]) < 3000
    proc = subprocess.run(
        ["bash", str(HOOK)], input="not json", capture_output=True, text=True
    )
    assert proc.returncode == 0 and "updatedToolOutput" not in proc.stdout
