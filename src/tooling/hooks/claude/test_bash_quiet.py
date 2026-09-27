"""The legacy Python quiet entrypoint forwards to native output bounding."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _bash_quiet as quiet


@pytest.fixture
def spill_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    path = tmp_path / "spill"
    monkeypatch.setattr(quiet, "SAVE_DIR", path)
    return path


def test_python_binding_spills_large_bash_output(spill_dir: Path) -> None:
    original = "line\n" * 9000
    result = quiet.hook_output({"tool_response": {"stdout": original, "stderr": ""}})
    bounded = result["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
    assert "[elided" in bounded
    assert [path.read_text() for path in spill_dir.iterdir()] == [original]


def test_legacy_shell_entrypoint_honors_codex_output_field(tmp_path: Path) -> None:
    hook = Path(__file__).resolve().with_name("post-bash-quiet.sh")
    payload = {"tool_response": {"stdout": "x\n" * 9000, "stderr": ""}}
    env = {
        **os.environ,
        "BASH_QUIET_SAVE_DIR": str(tmp_path),
        "BASH_QUIET_OUTPUT_FIELD": "updatedMCPToolOutput",
    }
    proc = subprocess.run(
        ["bash", str(hook)],
        input=json.dumps(payload),
        text=True,
        capture_output=True,
        env=env,
        check=True,
    )
    output = json.loads(proc.stdout)["hookSpecificOutput"]
    assert "[elided" in output["updatedMCPToolOutput"]["stdout"]
    assert "updatedToolOutput" not in output
