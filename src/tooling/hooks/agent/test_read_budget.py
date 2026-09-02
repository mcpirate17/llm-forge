"""Tests for the per-session read-token budget hook."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import read_budget as rb  # noqa: E402

HOOK = Path(__file__).resolve().parent / "read_budget.py"


def _read_payload(chars: int, session: str = "s1") -> dict:
    return {
        "session_id": session,
        "tool_name": "Read",
        "tool_response": {
            "type": "text",
            "file": {"filePath": "/x.py", "content": "a" * chars, "numLines": 1},
        },
    }


def test_response_chars_walks_nested_and_is_bounded() -> None:
    assert rb.response_chars({"a": "xx", "b": ["yyy", {"c": "z"}], "n": 5}) == 6
    assert rb.response_chars("abc") == 3
    assert rb.response_chars(None) == 0
    assert rb.response_chars({"a": "x" * 100, "b": "y" * 100}, limit=150) == 150


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        ("", 0),
        ("abcd", 4),
        (["ab", "cd"], 4),
        ({"a": "ab", "b": {"c": "cd"}, "n": 7, "z": None}, 4),
        ({"a": ["x" * 10, {"b": "y" * 10}]}, 20),
    ],
)
def test_response_chars_property(value: object, expected: int) -> None:
    assert rb.response_chars(value) == expected
    assert rb.response_chars(value, limit=3) == min(expected, 3)


def test_tally_accumulates_per_session(tmp_path: Path) -> None:
    assert rb.tally(tmp_path, "k", 10) == (0, 10)
    assert rb.tally(tmp_path, "k", 5) == (10, 15)
    assert rb.tally(tmp_path, "other", 1) == (0, 1)
    (tmp_path / "k.read-tokens").write_text("garbage", encoding="utf-8")
    assert rb.tally(tmp_path, "k", 2) == (0, 2)


def test_hook_nudges_only_when_crossing_a_step(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv(rb.STEP_ENV, "1000")
    quiet = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
    assert rb.hook_output(_read_payload(4 * 600), tmp_path) == quiet  # 600 tokens
    out = rb.hook_output(_read_payload(4 * 600), tmp_path)  # 1,200 crosses 1,000
    context = out["hookSpecificOutput"]["additionalContext"]
    assert context.startswith("READ BUDGET: 1,204 tokens")
    assert "crossed 1,000" in context and "locate_tool" in context
    assert rb.hook_output(_read_payload(4 * 100), tmp_path) == quiet  # 1,300
    out = rb.hook_output(_read_payload(4 * 800), tmp_path)  # 2,100 crosses 2,000
    assert "crossed 2,000" in out["hookSpecificOutput"]["additionalContext"]
    assert rb.hook_output(_read_payload(4 * 900, session="s2"), tmp_path) == quiet


def test_hook_ignores_sessionless_empty_and_malformed(tmp_path: Path) -> None:
    quiet = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
    assert (
        rb.hook_output({"tool_response": {"file": {"content": "x" * 4000}}}, tmp_path)
        == quiet
    )
    assert rb.hook_output({"session_id": "s", "tool_response": {}}, tmp_path) == quiet
    assert rb.hook_output("garbage", tmp_path) == quiet
    assert not list(tmp_path.iterdir())


def test_cli_round_trip(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    env = {"CRG_GATE_STATE_DIR": str(tmp_path), rb.STEP_ENV: "10"}
    proc = subprocess.run(
        [sys.executable, str(HOOK)],
        input=json.dumps(_read_payload(4 * 50)),
        capture_output=True,
        text=True,
        env={**dict(__import__("os").environ), **env},
        check=True,
    )
    out = json.loads(proc.stdout)["hookSpecificOutput"]
    assert out["additionalContext"].startswith("READ BUDGET: 52 tokens")
    proc = subprocess.run(
        [sys.executable, str(HOOK)], input="nope", capture_output=True, text=True
    )
    assert proc.returncode == 0 and "additionalContext" not in proc.stdout
