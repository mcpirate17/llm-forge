"""Differential parity twin (Python side) for the Rust port of the three Bash
`PreToolUse` hooks: `crg_gate.verify_bash`, `crg_graph_refresh.failure_output`,
and `conductor.current_work_guard`.

`native/forge/tests/bash_pretooluse_hooks_parity.rs` and this file load the
SAME two fixtures -- `bash_pretooluse_corpus.json` (64 case descriptors:
session, owner, command, claims with FIXED absolute timestamps, marker
content, or a bare payload, depending on the hook) and
`bash_pretooluse_expected.json` (frozen verdicts, captured once from these
very Python modules) -- and each independently rebuilds the filesystem state
a case needs (a `git init`'d repo, a claims store, a graph-used marker) before
asserting its own live implementation still matches the frozen values. That
pins both implementations to one shared ground truth instead of comparing
them to each other at test time.

Claim timestamps are fixed absolute dates rather than offsets from "now" --
see the Rust test's module doc for why that is required for the frozen
`claim_id` (a hash over the timestamps) to stay stable across runs.

The per-case dispatch (`_run_crg_gate_verify_bash` etc.) is imported directly
from `tests/fixtures/parity_driver.py`, which used to be spawned as a
subprocess by the Rust test; it is loaded here, unmodified, as this file's
only consumer now.
"""

from __future__ import annotations

import hashlib
import importlib
import importlib.util
import json
import os
import subprocess
import tempfile
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_FIXTURES = (
    _HERE.parent.parent.parent.parent / "native" / "forge" / "tests" / "fixtures"
)
_DRIVER_PATH = _FIXTURES / "parity_driver.py"

_MANAGED_ENV_VARS = (
    "CRG_GATE_REPO_ROOT",
    "PROJECT_DIR",
    "CRG_GATE_STATE_DIR",
    "CRG_DATA_DIR",
    "GOVERNANCE_OWNER",
    "LOCAL_AI_RUNTIME",
)


def _load_driver():
    spec = importlib.util.spec_from_file_location(
        "bash_pretooluse_parity_driver_under_test", _DRIVER_PATH
    )
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


driver = _load_driver()


def _reset_env() -> None:
    for key in _MANAGED_ENV_VARS:
        os.environ.pop(key, None)


def _load_json(name: str):
    return json.loads((_FIXTURES / name).read_text())


def _make_scratch(tmp_path: Path, label: str) -> Path:
    d = Path(tempfile.mkdtemp(prefix=f"{label}-", dir=tmp_path))
    return d


def _make_git_repo(tmp_path: Path, label: str) -> Path:
    root = _make_scratch(tmp_path, f"{label}-repo")
    subprocess.run(["git", "init", "-q"], cwd=root, check=True)
    return root


def _common_dir(root: Path) -> Path:
    return (root / ".git").resolve()


def _mark_graph_used(state_dir: Path, session_id: str) -> None:
    key = hashlib.sha256(session_id.encode()).hexdigest()
    state_dir.mkdir(parents=True, exist_ok=True)
    (state_dir / f"{key}.graph-used").write_bytes(b"1")


def _claim_id(owner: str, paths: list[str], created_at: str, expires_at: str) -> str:
    # Matches the corpus generator's (and the Rust test's) canonical-json +
    # sha256 claim id: keys sorted alphabetically, no whitespace.
    fields = {
        "created_at": created_at,
        "expires_at": expires_at,
        "justification": "because",
        "owner": owner,
        "paths": paths,
    }
    canonical = json.dumps(fields, separators=(",", ":"))
    digest = hashlib.sha256(canonical.encode()).hexdigest()
    return f"claim-{digest[:20]}"


def _write_claims(root: Path, claims: list[dict]) -> None:
    out = []
    for c in claims:
        out.append(
            {
                "claim_id": _claim_id(
                    c["owner"], c["paths"], c["created_at"], c["expires_at"]
                ),
                "owner": c["owner"],
                "paths": c["paths"],
                "justification": "because",
                "created_at": c["created_at"],
                "expires_at": c["expires_at"],
            }
        )
    gov = _common_dir(root) / "governance"
    gov.mkdir(parents=True, exist_ok=True)
    (gov / "ownership-claims.json").write_text(
        json.dumps({"schema_version": 1, "claims": out})
    )


def _detach_head(root: Path) -> None:
    (root / ".git" / "HEAD").write_text("0000000000000000000000000000000000000000\n")


def test_fixture_files_exist_and_are_shared_with_the_rust_test() -> None:
    corpus = _load_json("bash_pretooluse_corpus.json")
    expected = _load_json("bash_pretooluse_expected.json")
    assert len(corpus) == len(expected)
    assert len(corpus) >= 64, (
        f"expected at least 22 gate + 20 refresh + 22 guard = 64, got {len(corpus)}"
    )


def _run_gate_case(tmp_path: Path, case: dict):
    root = _make_git_repo(tmp_path, case["id"])
    state_dir = _make_scratch(tmp_path, f"{case['id']}-state")
    if case["graph_used_session"]:
        _mark_graph_used(state_dir, case["graph_used_session"])
    if case["claims"]:
        _write_claims(root, case["claims"])
    if case["detach_head"]:
        _detach_head(root)
    os.environ["CRG_GATE_REPO_ROOT"] = str(root)
    os.environ["CRG_GATE_STATE_DIR"] = str(state_dir)
    os.environ["GOVERNANCE_OWNER"] = case["owner"]
    payload = {
        "session_id": case["session_id"],
        "tool_name": "Bash",
        "tool_input": {"command": case["command"]},
    }
    import crg_gate as crg_gate_mod

    importlib.reload(crg_gate_mod)
    return driver._run_crg_gate_verify_bash(crg_gate_mod, payload)


def _run_refresh_case(tmp_path: Path, case: dict):
    root = _make_git_repo(tmp_path, case["id"])
    data_dir = _make_scratch(tmp_path, f"{case['id']}-data")
    if case["marker_lines"] is not None:
        (data_dir / "refresh.failed").write_text(case["marker_lines"])
    os.environ["CRG_GATE_REPO_ROOT"] = str(root)
    os.environ["CRG_DATA_DIR"] = str(data_dir)
    import crg_gate as crg_gate_mod

    importlib.reload(crg_gate_mod)
    import crg_graph_refresh as crg_refresh_mod

    importlib.reload(crg_refresh_mod)
    return driver._run_crg_refresh_report_pre(crg_refresh_mod, {})


def _run_guard_case(case: dict):
    if case["local_ai_runtime"] is not None:
        os.environ["LOCAL_AI_RUNTIME"] = case["local_ai_runtime"]
    return driver._run_current_work_guard_bash(case["payload"])


def test_python_hooks_match_the_frozen_corpus(tmp_path: Path) -> None:
    corpus = _load_json("bash_pretooluse_corpus.json")
    expected = _load_json("bash_pretooluse_expected.json")
    failures = []
    for case in corpus:
        _reset_env()
        hook = case["hook"]
        if hook == "crg_gate_verify_bash":
            verdict = _run_gate_case(tmp_path, case)
        elif hook == "crg_refresh_report_pre":
            verdict = _run_refresh_case(tmp_path, case)
        elif hook == "current_work_guard_bash":
            verdict = _run_guard_case(case)
        else:
            raise ValueError(f"unknown hook in corpus case: {hook!r}")
        _reset_env()
        expected_verdict = expected[case["id"]]
        if verdict != expected_verdict:
            failures.append(
                f"case {case['id']!r}: python={verdict!r} expected={expected_verdict!r}"
            )
    assert not failures, f"{len(failures)} parity mismatches:\n" + "\n".join(failures)
