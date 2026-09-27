"""The legacy read-budget entrypoint reaches native state accounting."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import read_budget as budget


def test_python_binding_counts_and_tallies_in_native_code(tmp_path: Path) -> None:
    assert budget.response_chars({"a": "xx", "b": ["yyy", "z"]}) == 6
    assert budget.tally(tmp_path, "session", 5) == (0, 5)
    assert budget.tally(tmp_path, "session", 7) == (5, 12)


def test_legacy_cli_emits_native_advice(tmp_path: Path) -> None:
    hook = Path(__file__).resolve().with_name("read_budget.py")
    payload = {"session_id": "s", "tool_response": "x" * 400}
    proc = subprocess.run(
        [sys.executable, str(hook)],
        input=json.dumps(payload),
        text=True,
        capture_output=True,
        check=True,
        env={
            **os.environ,
            "CRG_GATE_STATE_DIR": str(tmp_path),
            "READ_BUDGET_STEP_TOKENS": "50",
        },
    )
    assert (
        "READ BUDGET"
        in json.loads(proc.stdout)["hookSpecificOutput"]["additionalContext"]
    )
