"""Differential parity twin (Python side) for the Rust port of the
PostToolUse hooks behind the two zero-interpreter-start slices:
``crg_graph_refresh.failure_output`` (``crg_refresh_report_post``),
``crg_graph_refresh.full_update_output`` behind the git-tree-rewrite check
(``post_bash_graph``), ``read_budget.hook_output``,
``conductor.context_telemetry``'s two record builders, and the edit family
(``_post_edit_audit.hook_output`` for ``post_edit``,
``crg_graph_refresh.hook_output`` for ``crg_graph_refresh``, and
``obsidian_sync.cmd_post_edit`` for ``obsidian_post_edit``).

``native/forge/tests/post_tool_zero_start_parity.rs`` and this file load the
SAME two fixtures -- ``post_tool_corpus.json`` (44 case descriptors: a
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
* The ``post_edit`` cases keep the same scratch-bin trick for the
  formatters (stub ``ruff``/``rustfmt``), so no real formatter ever runs or
  rewrites a file under test.
* The ``obsidian_edit`` cases pin ``HOME``/vault/memory roots to scratch
  dirs, and the frozen verdicts normalize every scratch prefix (including
  the dashed *slug* form of the repo path the default memory root embeds)
  plus the accumulator timestamp and the mirror note's date line.
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib
import importlib.util
import io
import json
import os
import re
import sys
import tempfile
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_REPO = _HERE.parent.parent.parent.parent
_SRC = _REPO / "src"
_AGENT = _SRC / "tooling" / "hooks" / "agent"
_CLAUDE = _SRC / "tooling" / "hooks" / "claude"
sys.path.insert(0, str(_SRC))
sys.path.insert(0, str(_AGENT))
sys.path.insert(0, str(_CLAUDE))

import conductor.context_telemetry as telemetry  # noqa: E402

_FIXTURES = _REPO / "native" / "forge" / "tests" / "fixtures"
_STAMP = "2026-09-13T00:00:00.000+00:00"
_PID = 3831796
_DATE_STAMP = "2026-09-13"
_SLEEPER = ["/bin/sleep", "30"]
_ACCUM_DIR = Path("/tmp/claude-session-journal")
_OBS_LOADS = 0

_MANAGED_ENV_VARS = (
    "CRG_GATE_REPO_ROOT",
    "CRG_DATA_DIR",
    "CRG_GATE_STATE_DIR",
    "READ_BUDGET_STEP_TOKENS",
    "QWEN_PROJECT_DIR",
    "CONTEXT_TELEMETRY_PATH",
    "CLAUDE_PROJECT_DIR",
    "CONDUCTOR_SNAPSHOT_PYTHON",
    "PROJECT_DIR",
    "OBSIDIAN_VAULT_ROOT",
    "CLAUDE_MEMORY_ROOT",
    "HOME",
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
    assert len(corpus) >= 40, (
        f"expected 24 first-slice cases (4 report + 4 graph + 5 budget + "
        f"9 telemetry + 2 path) plus >= 15 edit-family cases, got {len(corpus)}"
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


def _install_stub_formatters(bin_dir: Path) -> None:
    bin_dir.mkdir(parents=True, exist_ok=True)
    for tool in ("ruff", "rustfmt"):
        stub = bin_dir / tool
        stub.write_text("#!/bin/sh\nexit 0\n")
        stub.chmod(stub.stat().st_mode | 0o111)


def _substitute(value, replacements: dict[str, str]):
    """Fill the corpus's ``<PLACEHOLDER>`` tokens with this run's paths."""
    if isinstance(value, str):
        for old, new in replacements.items():
            value = value.replace(old, new)
        return value
    if isinstance(value, dict):
        return {key: _substitute(item, replacements) for key, item in value.items()}
    if isinstance(value, list):
        return [_substitute(item, replacements) for item in value]
    return value


def _normalize_strings(obj, mapping: dict[str, str]):
    """Replace every scratch-root prefix in every string, recursively."""
    if isinstance(obj, str):
        for old, new in mapping.items():
            obj = obj.replace(old, new)
        return obj
    if isinstance(obj, dict):
        return {key: _normalize_strings(value, mapping) for key, value in obj.items()}
    if isinstance(obj, list):
        return [_normalize_strings(value, mapping) for value in obj]
    return obj


def _normalize_accum(text: str, mapping: dict[str, str]) -> str:
    ts, kind, fp = text.rstrip("\n").split("\t", 2)
    return "\t".join([_STAMP, kind, _normalize_strings(fp, mapping)])


def _normalize_mirror(note: str, mapping: dict[str, str]) -> str:
    dated = re.sub(r"^date: .*$", f"date: {_DATE_STAMP}", note, count=1, flags=re.M)
    return _normalize_strings(dated, mapping)


def _run_post_edit(tmp: Path, label: str, payload: dict, seed: dict, base_path: str) -> dict:
    import _post_edit_audit

    file_dir = tmp / f"{label}-file"
    file_dir.mkdir()
    path = file_dir / seed.get("file", "ghost.py")
    if seed.get("content") is not None:
        path.write_text(seed["content"])
    bin_dir = tmp / f"{label}-bin"
    _install_stub_formatters(bin_dir)
    payload_run = _substitute(payload, {"<FILE>": str(path)})
    # The scratch bin dir alone holds the formatters (stubs on every
    # machine), exactly as the generator pinned it.
    os.environ["PATH"] = str(bin_dir)
    output = _post_edit_audit.hook_output(payload_run)
    os.environ["PATH"] = base_path
    return {"output": _normalize_strings(output, {str(file_dir): "<F>"})}


def _run_graph_edit(tmp: Path, label: str, payload: dict, seed: dict, base_path: str) -> dict:
    import crg_gate as crg_gate_mod
    import crg_graph_refresh as crg_refresh_mod

    repo = _make_repo(tmp, label)
    for rel, content in seed.get("files", {}).items():
        (repo / rel).write_text(content)
    outside = tmp / f"{label}-outside"
    outside.mkdir()
    (outside / "x.py").write_text("y = 2\n")
    bin_dir = tmp / f"{label}-bin"
    if seed.get("stub_tool"):
        _install_stub_tool(bin_dir)
    else:
        bin_dir.mkdir(parents=True, exist_ok=True)
    store = tmp / f"{label}-crgdata"
    store.mkdir()
    payload_run = _substitute(payload, {"<OUTSIDE>": str(outside / "x.py")})
    os.environ["PATH"] = str(bin_dir)
    os.environ["CRG_GATE_REPO_ROOT"] = str(repo)
    os.environ["CRG_DATA_DIR"] = str(store)
    importlib.reload(crg_gate_mod)
    importlib.reload(crg_refresh_mod)
    # The sleeper stands in for the real worker (positional parameters mirror
    # `worker_command(body, root)`), so the spawn happens without a refresh.
    crg_refresh_mod.worker_command = lambda _body, _root: list(_SLEEPER)
    output = crg_refresh_mod.hook_output(payload_run)
    os.environ["PATH"] = base_path
    pending = None
    if (store / "refresh.pending").exists():
        pending = (store / "refresh.pending").read_text()
    return {"output": output, "pending": pending}


def _run_obsidian_edit(tmp: Path, case: dict) -> dict:
    label = case["id"]
    payload = case["payload"]
    seed = case.get("seed", {})
    repo = tmp / f"{label}-repo"
    repo.mkdir()
    vault = tmp / f"{label}-vault"
    mem = tmp / f"{label}-mem"
    home = tmp / f"{label}-home"
    for directory in (vault, mem, home):
        directory.mkdir()
    base_by_where = {
        "repo": repo,
        "mem": mem,
        "mem-default": home
        / ".claude"
        / "projects"
        / (str(repo).replace("/", "-").replace("_", "-").replace(".", "-"))
        / "memory",
    }
    base = base_by_where[seed.get("where", "repo")]
    payload_run = payload
    if seed.get("file"):
        base.mkdir(parents=True, exist_ok=True)
        path = base / seed["file"]
        if seed.get("content") is not None:
            path.write_text(seed["content"])
        payload_run = _substitute(payload, {"<FILE>": str(path)})
    replacements = {
        "<VAULT>": str(vault),
        "<MEM>": str(mem),
        "<HOME>": str(home),
    }
    resolved_env = {k: replacements[v] for k, v in case.get("env", {}).items()}
    resolved_env.setdefault("CLAUDE_PROJECT_DIR", str(repo))
    for key, value in resolved_env.items():
        os.environ[key] = value
    # A fresh module per case: the module-level roots re-read the env.
    global _OBS_LOADS
    _OBS_LOADS += 1
    spec = importlib.util.spec_from_file_location(
        f"obsidian_sync_twin_{_OBS_LOADS}", _CLAUDE / "obsidian_sync.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    accum_path = _ACCUM_DIR / f"{payload['session_id']}.tsv"
    accum_path.unlink(missing_ok=True)
    buffer = io.StringIO()
    old_stdin = sys.stdin
    sys.stdin = io.StringIO(json.dumps(payload_run))
    try:
        with contextlib.redirect_stdout(buffer):
            module.cmd_post_edit()
    finally:
        sys.stdin = old_stdin
    text = buffer.getvalue().strip()
    output = json.loads(text) if text else None
    mapping = {
        str(home): "<H>",
        str(vault): "<V>",
        str(mem): "<M>",
        str(repo): "<R>",
        str(repo).replace("/", "-").replace("_", "-").replace(".", "-"): "<RS>",
    }
    accum = None
    if accum_path.exists():
        accum = _normalize_accum(accum_path.read_text(), mapping)
        accum_path.unlink()
    mirror_path = None
    mirror = None
    notes_dir = module.VAULT_ROOT / "memory"
    if notes_dir.is_dir():
        notes = sorted(notes_dir.glob("*.md"))
        if notes:
            mirror_path = notes[0].relative_to(module.VAULT_ROOT).as_posix()
            mirror = _normalize_mirror(notes[0].read_text(), mapping)
    return {
        "output": output,
        "accum": accum,
        "mirror_path": mirror_path,
        "mirror": mirror,
    }


def _run_report_post(tmp: Path, label: str, seed: dict) -> dict:
    import crg_gate as crg_gate_mod
    import crg_graph_refresh as crg_refresh_mod

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


def _run_graph_bash(tmp: Path, label: str, payload: dict, seed: dict, base_path: str) -> dict:
    import crg_gate as crg_gate_mod
    import crg_graph_refresh as crg_refresh_mod

    from tooling.hooks.dispatch.adapters import GIT_TREE_REWRITE

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
        str(tool_input.get("command") or "") if isinstance(tool_input, dict) else ""
    )
    if not command or GIT_TREE_REWRITE.search(command) is None:
        output = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
    else:
        output = crg_refresh_mod.full_update_output()
    os.environ["PATH"] = base_path
    pending_path = store / "refresh.pending"
    pending = pending_path.read_text() if pending_path.exists() else None
    return {"output": output, "pending": pending}


def _run_read_budget(tmp: Path, label: str, payload: dict, seed: dict) -> dict:
    import crg_gate as crg_gate_mod
    import read_budget

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


def _run_telemetry_path(case: dict) -> dict:
    # `adapters._telemetry_path` verbatim: the env override, else the
    # module's own DEFAULT_PATH (under the ledger root -- `LEDGER_ROOT`,
    # else /mnt/data/llm/ledger).
    path = Path(
        os.environ.get("CONTEXT_TELEMETRY_PATH", str(telemetry.DEFAULT_PATH))
    )
    if case.get("env", {}).get("CONTEXT_TELEMETRY_PATH"):
        return {"path": str(path)}
    try:
        suffix = str(path.relative_to(_REPO))
    except ValueError:
        # The default lives outside the checkout, under a per-machine
        # ledger root; the shape both twins freeze is the three
        # components under that root (telemetry/context_telemetry/
        # events.jsonl) -- the Rust twin pins the same last three.
        suffix = str(Path(*path.parts[-3:]))
    return {"path_suffix": suffix}


def _run_case(tmp: Path, case: dict, base_path: str):
    """Rebuild one case's state and compute its live verdict, field by field.

    Mirrors the fixture generator's own per-kind branches exactly (same
    reloads, same env, same monkeypatch) so whatever semantics produced the
    frozen values hold here too. The per-kind bodies live in their own
    helpers -- the complexity ratchet caps this dispatcher well below grade D.
    """
    kind = case["kind"]
    payload = case["payload"]
    seed = case.get("seed", {})
    label = case["id"]
    _reset_env()
    if kind == "obsidian_edit":
        # Its env values are placeholders the helper itself resolves.
        return _run_obsidian_edit(tmp, case)
    for key, value in case.get("env", {}).items():
        os.environ[key] = value

    if kind == "report_post":
        return _run_report_post(tmp, label, seed)
    if kind == "graph_bash":
        return _run_graph_bash(tmp, label, payload, seed, base_path)
    if kind == "read_budget":
        return _run_read_budget(tmp, label, payload, seed)
    if kind == "telemetry_record":
        return {"line": _pinned_event(payload)}
    if kind == "telemetry_hook_context":
        return {
            "line": _pinned_hook_context(
                seed["hook"], seed["hook_json"], payload["session_id"]
            )
        }
    if kind == "telemetry_path":
        return _run_telemetry_path(case)
    if kind == "post_edit":
        return _run_post_edit(tmp, label, payload, seed, base_path)
    if kind == "graph_edit":
        return _run_graph_edit(tmp, label, payload, seed, base_path)
    raise ValueError(f"unknown kind in corpus case {label!r}: {kind!r}")


def test_python_hooks_match_the_frozen_corpus(tmp_path: Path) -> None:
    corpus = _load_json("post_tool_corpus.json")
    expected = _load_json("post_tool_expected.json")
    base_path = os.environ["PATH"]
    base_home = os.environ.get("HOME")
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
        if base_home is not None:
            os.environ["HOME"] = base_home
    assert not failures, (
        f"{len(failures)} parity mismatches:\n" + "\n".join(failures)
    )
