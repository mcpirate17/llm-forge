#!/usr/bin/env python3
"""Batch, fresh-per-case driver for the Bash PreToolUse parity corpus.

Reads one JSON array from stdin -- each element `{"hook", "payload", "env"}`
-- and prints one JSON array of results to stdout, one verdict per case (an
object, or `null` for "no opinion": nothing to print).

Everything runs in ONE interpreter for speed, but `crg_gate`'s (and, through
its `from crg_gate import REPO_ROOT`, `crg_graph_refresh`'s) module-level
`REPO_ROOT` / `_REPO_CHECKOUT` / `REPO_COMMON_DIR` are computed once at
*import* time from `CRG_GATE_REPO_ROOT`. A case that changes that env var
must force those constants to be recomputed, or it would silently observe an
earlier case's frozen values -- exactly what a real, short-lived hook
subprocess never does (it only ever sees one case). `importlib.reload(...)`
before every crg_gate/crg_refresh case re-executes the module body against
the case's own env, standing in for "one fresh process per case" without
actually paying for one.

`current_work_guard` carries no such module-level state (its one env read,
`LOCAL_AI_RUNTIME`, happens inside the function call itself), so those cases
need no reload -- setting the env var before the call is enough.

Managed env vars are reset (set, or deleted if the case omits them) before
every case, so no case can leak state into the next one.
"""

from __future__ import annotations

import importlib
import io
import json
import os
import sys
from contextlib import redirect_stdout
from pathlib import Path

_THIS = Path(__file__).resolve()
_REPO_SRC = _THIS.parents[4] / "src"
_AGENT_DIR = _REPO_SRC / "tooling" / "hooks" / "agent"

sys.path.insert(0, str(_REPO_SRC))
sys.path.insert(0, str(_AGENT_DIR))

_MANAGED_ENV_VARS = (
    "CRG_GATE_REPO_ROOT",
    "PROJECT_DIR",
    "CRG_GATE_STATE_DIR",
    "CRG_DATA_DIR",
    "GOVERNANCE_OWNER",
    "LOCAL_AI_RUNTIME",
)


def _apply_env(env: dict[str, str]) -> None:
    for key in _MANAGED_ENV_VARS:
        if key in env:
            os.environ[key] = env[key]
        else:
            os.environ.pop(key, None)


def _run_crg_gate_verify_bash(crg_gate, payload: dict) -> dict | None:
    from conductor.candidate_review.identity import OwnerIdentityError, resolve_owner

    session_root = crg_gate.session_checkout(payload)
    try:
        owner = resolve_owner(session_root)
    except OwnerIdentityError:
        owner = ""
    buf = io.StringIO()
    with redirect_stdout(buf):
        crg_gate.verify_bash(payload, owner=owner)
    text = buf.getvalue().strip()
    return json.loads(text) if text else None


def _run_crg_refresh_report_pre(crg_graph_refresh, payload: dict) -> dict | None:
    return crg_graph_refresh.failure_output("PreToolUse")


def _run_current_work_guard_bash(payload: dict) -> dict | None:
    from conductor.current_work_guard import (
        advisory_for_payload,
        evaluate_payload,
        hook_protocol,
        hook_response,
    )

    protocol = hook_protocol(payload)
    return hook_response(
        evaluate_payload(payload),
        protocol=protocol,
        advisory=advisory_for_payload(payload),
    )


def main() -> int:
    cases = json.load(sys.stdin)
    results: list[dict | None] = []

    crg_gate = None
    crg_graph_refresh = None

    for case in cases:
        hook = case["hook"]
        payload = case["payload"]
        _apply_env(case.get("env", {}))

        if hook == "crg_gate_verify_bash":
            if crg_gate is None:
                import crg_gate as _cg

                crg_gate = _cg
            else:
                importlib.reload(crg_gate)
            results.append(_run_crg_gate_verify_bash(crg_gate, payload))
        elif hook == "crg_refresh_report_pre":
            if crg_gate is None:
                import crg_gate as _cg

                crg_gate = _cg
            else:
                importlib.reload(crg_gate)
            if crg_graph_refresh is None:
                import crg_graph_refresh as _cgr

                crg_graph_refresh = _cgr
            else:
                importlib.reload(crg_graph_refresh)
            results.append(_run_crg_refresh_report_pre(crg_graph_refresh, payload))
        elif hook == "current_work_guard_bash":
            results.append(_run_current_work_guard_bash(payload))
        else:
            raise ValueError(f"unknown hook in corpus case: {hook!r}")

    print(json.dumps(results))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
