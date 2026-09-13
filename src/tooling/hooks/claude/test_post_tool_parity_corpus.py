"""Differential parity twin (Python side) for the Rust port of the four
PostToolUse hooks behind the zero-interpreter-start slice:
``crg_graph_refresh.failure_output`` (``crg_refresh_report_post``),
``crg_graph_refresh.full_update_output`` behind the git-tree-rewrite check
(``post_bash_graph``), ``read_budget.hook_output``, and
``conductor.context_telemetry``'s two record builders.

``native/forge/tests/post_tool_zero_start_parity.rs`` and this file load the
SAME two fixtures -- ``post_tool_corpus.json`` (24 case descriptors: a
``kind`` discriminator, the raw hook payload, optional env overrides and seed
state) and ``post_tool_expected.json`` (frozen verdicts, captured once from
these very Python modules) -- and each independently rebuilds the state a
case needs before asserting its own live implementation still matches the
frozen values. That pins both implementations to one shared ground truth
instead of comparing them to each other at test time (the shape
``test_bash_pretooluse_hooks_parity_corpus.py`` established).

Determinism notes, mirrored from the fixture generator:

* Telemetry records pin the two volatile fields by overwriting
  ``timestamp``/``pid`` in the dict the real ``event``/``hook_context_event``
  builders returned (a value overwrite never reorders a dict) before encoding
  with the module's own ``_encoded_record`` -- the frozen line is the real
  builder's byte output, not a reimplementation.
* The graph-queue cases point ``PATH`` at a scratch bin dir alone (a stub
  ``code-review-graph`` for the tool-present case, an empty one for the
  absent case) so the host's real tool can never leak into a verdict, and the
  spawned "worker" is monkeypatched to ``/bin/sleep 30`` -- the same inert
  stand-in the generator used, so the spawn really happens without running
  any refresh.
"""

from __future__ import annotations

import hashlib
import importlib
import json
import os
import sys
import tempfile
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_REPO = _HERE.parent.parent.parent.parent
_SRC = _REPO / "src"
_AGENT = _SRC / "tooling" / "hooks" / "agent"
sys.path.insert(0, str(_SRC))
sys.path.insert(0, str(_AGENT))

import conductor.context_telemetry as telemetry  # noqa: E402

_FIXTURES = _REPO / "native" / "forge" / "tests" / "fixtures"
_STAMP = "2026-09-13T00:00:00.000+00:00"
_PID = 3831796
_SLEEPER = ["/bin/sleep", "30"]

_MANAGED_ENV_VARS = (
    "CRG_GATE_REPO_ROOT",
    "CRG_DATA_DIR",
    "CRG_GATE_STATE_DIR",
    "READ_BUDGET_STEP_TOKENS",
    "QWEN_PROJECT_DIR",
    "CONTEXT_TELEMETRY_PATH",
    "CLAUDE_PROJECT_DIR",
    "CONDUCTOR_SNAPSHOT_PYTHON",
)


def _reset_env() -> None:
    for key in _MANAGED_ENV_VARS:
        os.environ.pop(key, None)


def _load_json(name: str):
    return json.loads((_FIXTURES / name).read_text())


def test_fixture_files_exist_and_are_shared_with_the_rust_test() -> None:
    corpus = _load_json("post_tool_corpus.json")
    expected = _load_json("post_tool_expected.json")
    assert len(corpus) == len(expected)
    assert len(corpus) >= 20, (
        f"expected 4 report + 4 graph + 5 budget + 9 telemetry + 2 path = 24, "
        f"got {len(corpus)}"
    )


def _ledger_key(session_id: str) -> str:
    return hashlib.sha256(session_id.encode()).hexdigest()


def _pinned_event(payload: dict) -> str:
    item = telemetry.event(payload)
    item["timestamp"] = _STAMP
    item["pid"] = _PID
    return telemetry._encoded_record(item).decode("utf-8")


def _pinned_hook_context(hook: str, hook_json: dict, session_id: str) -> str:
    item = telemetry.hook_context_event(hook, hook_json, session_id=session_id)
    item["timestamp"] = _STAMP
    item["pid"] = _PID
    return telemetry._encoded_record(item).decode("utf-8")


def _make_repo(tmp: Path, label: str) -> Path:
    repo = tmp / f"{label}-repo"
    (repo / ".git").mkdir(parents=True)
    (repo / ".git/HEAD").write_text("ref: refs/heads/lane\n")
    return repo


def _install_stub_tool(bin_dir: Path) -> None:
    bin_dir.mkdir(parents=True, exist_ok=True)
    stub = bin_dir / "code-review-graph"
    stub.write_text("#!/bin/sh\nexit 0\n")
    stub.chmod(stub.stat().st_mode | 0o111)


def _run_case(tmp: Path, case: dict, base_path: str):
    """Rebuild one case's state and compute its live verdict, field by field.

    Mirrors the fixture generator's own per-kind branches exactly (same
    reloads, same env, same monkeypatch) so whatever semantics produced the
    frozen values hold here too.
    """
    import crg_gate as crg_gate_mod
    import crg_graph_refresh as crg_refresh_mod
    import read_budget

    from tooling.hooks.dispatch.adapters import GIT_TREE_REWRITE

    kind = case["kind"]
    payload = case["payload"]
    seed = case.get("seed", {})
    label = case["id"]
    _reset_env()
    for key, value in case.get("env", {}).items():
        os.environ[key] = value

    if kind == "report_post":
        store = tmp / f"{label}-store"
        store.mkdir()
        if seed.get("refresh_failed"):
            (store / "refresh.failed").write_text(seed["refresh_failed"])
        os.environ["CRG_DATA_DIR"] = str(store)
        importlib.reload(crg_gate_mod)
        importlib.reload(crg_refresh_mod)
        output = crg_refresh_mod.failure_output("PostToolUse")
        failed_after = (
            (store / "refresh.failed").read_text()
            if (store / "refresh.failed").exists()
            else None
        )
        return {"output": output, "failed_after": failed_after}

    if kind == "graph_bash":
        repo = _make_repo(tmp, label)
        bin_dir = tmp / f"{label}-bin"
        if seed.get("stub_tool"):
            _install_stub_tool(bin_dir)
        else:
            bin_dir.mkdir(parents=True, exist_ok=True)
        store = tmp / f"{label}-crgdata"
        store.mkdir()
        # The scratch bin dir alone decides whether code-review-graph is
        # installed, exactly as the generator pinned it.
        os.environ["PATH"] = str(bin_dir)
        os.environ["CRG_GATE_REPO_ROOT"] = str(repo)
        os.environ["CRG_DATA_DIR"] = str(store)
        importlib.reload(crg_gate_mod)
        importlib.reload(crg_refresh_mod)
        # The sleeper stands in for the real worker so the spawn is
        # observable and inert; identical to the generator's patch. The
        # positional parameters mirror `worker_command(body, root)` -- the
        # call site passes both, the stand-in uses neither.
        crg_refresh_mod.worker_command = lambda _body, _root: list(_SLEEPER)
        tool_input = payload.get("tool_input")
        command = (
            str(tool_input.get("command") or "")
            if isinstance(tool_input, dict)
            else ""
        )
        if not command or GIT_TREE_REWRITE.search(command) is None:
            output = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
        else:
            output = crg_refresh_mod.full_update_output()
        os.environ["PATH"] = base_path
        pending_path = store / "refresh.pending"
        pending = pending_path.read_text() if pending_path.exists() else None
        return {"output": output, "pending": pending}

    if kind == "read_budget":
        gate = tmp / f"{label}-gate"
        gate.mkdir()
        if seed.get("ledger") is not None:
            key = _ledger_key(payload["session_id"])
            (gate / f"{key}.read-tokens").write_text(seed["ledger"] + "\n")
        os.environ["CRG_GATE_STATE_DIR"] = str(gate)
        importlib.reload(crg_gate_mod)
        state_dir = crg_gate_mod._state_dir()
        output = read_budget.hook_output(payload, state_dir)
        ledger_after = None
        if payload.get("session_id"):
            path = state_dir / f"{_ledger_key(payload['session_id'])}.read-tokens"
            ledger_after = path.read_text() if path.exists() else None
        return {"output": output, "ledger_after": ledger_after}

    if kind == "telemetry_record":
        return {"line": _pinned_event(payload)}

    if kind == "telemetry_hook_context":
        return {
            "line": _pinned_hook_context(
                seed["hook"], seed["hook_json"], payload["session_id"]
            )
        }

    if kind == "telemetry_path":
        # `adapters._telemetry_path` verbatim: the env override, else the
        # module's own DEFAULT_PATH (derived from the module file's location
        # -- the inherited `<checkout>/src/research` quirk).
        path = Path(
            os.environ.get("CONTEXT_TELEMETRY_PATH", str(telemetry.DEFAULT_PATH))
        )
        if case.get("env", {}).get("CONTEXT_TELEMETRY_PATH"):
            return {"path": str(path)}
        try:
            suffix = str(path.relative_to(_REPO))
        except ValueError:
            # A conductor already imported from elsewhere (site-packages)
            # pins DEFAULT_PATH under its own root; the inherited suffix
            # shape -- src/research/tmp/context_telemetry/events.jsonl -- is
            # what both twins freeze, and the Rust one asserts it under its
            # own scratch root via strip_prefix.
            suffix = str(Path(*path.parts[-5:]))
        return {"path_suffix": suffix}

    raise ValueError(f"unknown kind in corpus case {label!r}: {kind!r}")


def test_python_hooks_match_the_frozen_corpus(tmp_path: Path) -> None:
    corpus = _load_json("post_tool_corpus.json")
    expected = _load_json("post_tool_expected.json")
    base_path = os.environ["PATH"]
    tmp = Path(tempfile.mkdtemp(prefix="pt-twin-", dir=tmp_path))
    failures: list[str] = []
    try:
        for case in corpus:
            label = case["id"]
            live = _run_case(tmp, case, base_path)
            _reset_env()
            frozen = expected[label]
            for field, value in live.items():
                want = frozen.get(field)
                if field == "path_suffix":
                    # The checkout root differs per machine (and per install);
                    # the inherited suffix shape is what is frozen. The Rust
                    # twin pins the same suffix under its own scratch root.
                    if not str(value).endswith(want):
                        failures.append(
                            f"case {label!r} (path_suffix): "
                            f"python={value!r} expected={want!r}"
                        )
                elif value != want:
                    failures.append(
                        f"case {label!r} ({field}): "
                        f"python={value!r} expected={want!r}"
                    )
    finally:
        _reset_env()
        os.environ["PATH"] = base_path
    assert not failures, (
        f"{len(failures)} parity mismatches:\n" + "\n".join(failures)
    )
