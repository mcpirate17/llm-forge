"""The legacy Python Read/Grep/MCP quiet entrypoint reaches forge."""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _bash_quiet as bash_quiet
import post_tool_quiet as quiet

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "test_post_tool_quiet"


def payload(name: str) -> dict:
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


def test_python_binding_returns_native_read_resume_marker() -> None:
    output = quiet.hook_output(payload("read_over_cap.json"))
    content = output["hookSpecificOutput"]["updatedToolOutput"]["file"]["content"]
    assert "Read(offset=61, limit=...)" in content


def test_python_binding_reports_unrecognized_shape(
    capsys: pytest.CaptureFixture[str],
) -> None:
    output = quiet.hook_output(payload("malformed_shape.json"))
    assert output == {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
    assert "unrecognized tool_response shape" in capsys.readouterr().err


def test_shared_bash_configuration_is_used(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(bash_quiet, "SAVE_DIR", tmp_path)
    output = quiet.hook_output(payload("grep_over_cap.json"))
    assert "full output:" in output["hookSpecificOutput"]["updatedToolOutput"]
    assert len(list(tmp_path.iterdir())) == 1
