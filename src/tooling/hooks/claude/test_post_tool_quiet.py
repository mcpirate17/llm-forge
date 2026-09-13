"""Tests for the PostToolUse Read/Grep/MCP output bounding hook.

The payloads are plain JSON files under ``fixtures/test_post_tool_quiet/``:
this module is the reference implementation for the Rust port in
``native/forge``, which uses these fixtures as its parity corpus.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _bash_quiet as bq  # noqa: E402
import post_tool_quiet as ptq  # noqa: E402

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "test_post_tool_quiet"
NO_REWRITE = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}


def _payload(name: str) -> dict:
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


@pytest.fixture(autouse=True)
def _save_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setattr(bq, "SAVE_DIR", tmp_path / "out")
    return tmp_path / "out"


def test_below_cap_passes_through_with_no_rewrite() -> None:
    for name in ("read_under_cap.json",):
        assert ptq.hook_output(_payload(name)) == NO_REWRITE


def test_read_content_is_bounded_with_a_usable_resume_offset() -> None:
    out = ptq.hook_output(_payload("read_over_cap.json"))
    rewrite = out["hookSpecificOutput"]["updatedToolOutput"]
    content = rewrite["file"]["content"]
    original = _payload("read_over_cap.json")["tool_response"]["file"]["content"]
    assert len(content.encode()) < len(original.encode())
    assert content.startswith(original.splitlines(keepends=True)[0])
    assert content.endswith(original.splitlines(keepends=True)[-1])
    # 200 lines over the cap: the head keeps bq.HEAD_LINES whole lines, so the
    # first elided line is HEAD_LINES + 1 and Read(offset=...) reaches it.
    assert f"Read(offset={bq.HEAD_LINES + 1}, limit=...)" in content
    assert "(line 61)" in content
    marker = [ln for ln in content.splitlines() if "elided" in ln][0]
    assert "bytes at byte " in marker


def test_read_byte_split_names_the_cut_byte() -> None:
    out = ptq.hook_output(_payload("read_byte_split.json"))
    content = out["hookSpecificOutput"]["updatedToolOutput"]["file"]["content"]
    assert "bytes at byte 8" in content  # cap // 2 = 8000 bytes of head
    assert "Read(offset=1, limit=...)" in content  # no complete line before the cut


def test_grep_spill_file_holds_the_full_text(_save_dir: Path) -> None:
    payload = _payload("grep_over_cap.json")
    original = payload["tool_response"]
    out = ptq.hook_output(payload)
    rewrite = out["hookSpecificOutput"]["updatedToolOutput"]
    assert isinstance(rewrite, str) and "full output: " in rewrite
    spilled = sorted(_save_dir.glob("*.txt"))
    assert len(spilled) == 1 and spilled[0].read_text(encoding="utf-8") == original


def test_mcp_blocks_keep_shape_and_small_blocks_pass_through() -> None:
    out = ptq.hook_output(_payload("mcp_blocks_over_cap.json"))
    rewrite = out["hookSpecificOutput"]["updatedToolOutput"]
    assert isinstance(rewrite, list) and len(rewrite) == 2
    assert rewrite[1]["text"] == "small block"
    assert rewrite[0]["type"] == "text"
    assert "full output: " in rewrite[0]["text"]
    assert len(rewrite[0]["text"]) < 20000


def test_cap_zero_disables_bounding(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("TOOL_OUTPUT_QUIET_BYTES", "0")
    for name in (
        "read_over_cap.json",
        "grep_over_cap.json",
        "mcp_blocks_over_cap.json",
    ):
        assert ptq.hook_output(_payload(name)) == NO_REWRITE


def test_malformed_shape_passes_through_with_a_warning(
    capsys: pytest.CaptureFixture[str],
) -> None:
    assert ptq.hook_output(_payload("malformed_shape.json")) == NO_REWRITE
    err = capsys.readouterr().err
    assert "unrecognized tool_response shape" in err
