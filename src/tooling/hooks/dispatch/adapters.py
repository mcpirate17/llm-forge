"""In-process adapters: one function per registered hook body.

Each adapter takes the dispatch ``Context`` and either returns the hook's JSON
as a dict, or returns ``None`` after the body printed its JSON to the (thread-
local) stdout. Bodies are imported from their files at first use — never at
module scope — so a broken body fails only the hook that needs it, loudly.
"""

from __future__ import annotations

import importlib
import importlib.util
import os
import re
import shutil
import subprocess
import sys
import threading
from pathlib import Path
from types import ModuleType
from typing import Any

_LOCK = threading.Lock()
_MODULES: dict[str, ModuleType] = {}
_PROJECT_ENV_LOADED: set[Path] = set()

ALLOW: dict[str, Any] = {
    "hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow"}
}
QUIET_PRE: dict[str, Any] = {"hookSpecificOutput": {"hookEventName": "PreToolUse"}}
QUIET_POST: dict[str, Any] = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
GIT_TREE_REWRITE = re.compile(
    r"git\s+(checkout|switch|merge|rebase|stash|pull|reset|cherry-pick|revert|apply|am)\b"
)


def _body(ctx: Any, relative: str) -> ModuleType:
    """Import a hook body by file path, once per process."""
    path = ctx.root / relative
    name = f"_hook_body_{path.stem.strip('_')}"
    with _LOCK:
        module = _MODULES.get(name)
    if module is not None:
        return module
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load hook body {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    with _LOCK:
        module = _MODULES.setdefault(name, module)
        sys.modules[name] = module
    return module


def _conductor(ctx: Any, module: str) -> ModuleType:
    root = str(ctx.root)
    if root not in sys.path:
        sys.path.insert(0, root)
    return importlib.import_module(module)


def _command(ctx: Any) -> str:
    tool_input = ctx.payload.get("tool_input")
    if not isinstance(tool_input, dict):
        return ""
    return str(tool_input.get("command") or "")


def _owner() -> str:
    return os.environ.get("GOVERNANCE_OWNER", "").strip() or "claude"


def _telemetry_path(telemetry: ModuleType) -> Path:
    DEFAULT_PATH = telemetry.DEFAULT_PATH
    return Path(os.environ.get("CONTEXT_TELEMETRY_PATH", DEFAULT_PATH))


def _telemetry(ctx: Any, hook: str, output: dict[str, Any]) -> None:
    """What ``context_telemetry hook-context`` logs: the injected context size."""
    telemetry = _conductor(ctx, "conductor.context_telemetry")
    path = _telemetry_path(telemetry)
    try:
        item = telemetry.hook_context_event(hook, output)
        if item["output_bytes"]:
            telemetry.record(item, path)
    except (OSError, TypeError, ValueError) as exc:
        print(f"context telemetry unavailable: {exc}", file=sys.stderr)


def _source_project_env(ctx: Any) -> None:
    """Apply ``.claude/hooks/project/env.sh`` exactly as a sourcing shell would."""
    hook_dir = Path(
        os.environ.get("PROJECT_HOOK_DIR") or ctx.root / ".claude/hooks/project"
    )
    env_sh = hook_dir / "env.sh"
    with _LOCK:
        if env_sh in _PROJECT_ENV_LOADED:
            return
        _PROJECT_ENV_LOADED.add(env_sh)
    if not os.access(env_sh, os.R_OK):
        return
    proc = subprocess.run(
        ["bash", "-c", 'source "$1" >/dev/null 2>&1; env -0', "_", str(env_sh)],
        capture_output=True,
        cwd=ctx.root,
        env=ctx.env,
        timeout=5,
        check=True,
    )
    for entry in proc.stdout.split(b"\0"):
        key, sep, value = entry.decode("utf-8", "replace").partition("=")
        if sep and os.environ.get(key) != value:
            os.environ[key] = value


# ── PreToolUse ──────────────────────────────────────────────────────────


def crg_gate_mark(ctx: Any) -> None:
    _body(ctx, "tooling/hooks/agent/crg_gate.py").mark(ctx.payload)


def crg_gate_verify(ctx: Any) -> None:
    _body(ctx, "tooling/hooks/agent/crg_gate.py").verify(ctx.payload, owner=_owner())


def crg_gate_verify_bash(ctx: Any) -> None:
    _body(ctx, "tooling/hooks/agent/crg_gate.py").verify_bash(
        ctx.payload, owner=_owner()
    )


def pre_bash(ctx: Any) -> dict[str, Any] | None:
    command = _command(ctx)
    if not command:
        return ALLOW
    reason = _body(ctx, "tooling/hooks/claude/_bash_guard.py").check(command)
    if reason:
        return {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason.strip(),
            }
        }
    impact = _body(ctx, "tooling/hooks/claude/_bash_impact.py")
    try:
        impact.main()
    except SystemExit as exc:
        if exc.code not in (None, 0):
            return ALLOW
    except Exception as exc:  # noqa: BLE001 - the shell allowed on any impact failure
        return {**ALLOW, "systemMessage": f"[pre_bash] impact analyzer failed: {exc!r}"}
    return None


def current_work_guard(ctx: Any) -> None:
    _conductor(ctx, "conductor.current_work_guard").main()


def pre_read_skeleton(ctx: Any) -> dict[str, Any]:
    output = _body(ctx, "tooling/hooks/claude/_pre_read_skeleton.py").hook_output(
        ctx.payload
    )
    _telemetry(ctx, "pre-read-skeleton", output)
    return output


# ── PostToolUse ─────────────────────────────────────────────────────────


def post_edit(ctx: Any) -> dict[str, Any]:
    return _body(ctx, "tooling/hooks/claude/_post_edit_audit.py").hook_output(
        ctx.payload
    )


def read_budget(ctx: Any) -> dict[str, Any]:
    body = _body(ctx, "tooling/hooks/agent/read_budget.py")
    return body.hook_output(ctx.payload, body._state_dir())


def obsidian_post_edit(ctx: Any) -> None:
    _body(ctx, "tooling/hooks/claude/obsidian_sync.py").cmd_post_edit()


def post_bash_graph(ctx: Any) -> dict[str, Any]:
    command = _command(ctx)
    if not command or GIT_TREE_REWRITE.search(command) is None:
        return QUIET_POST
    if shutil.which("code-review-graph") is None:
        return QUIET_POST
    proc = subprocess.run(
        ["code-review-graph", "update", "--skip-flows"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        cwd=ctx.root,
        env=ctx.env,
        timeout=59,
        check=False,
    )
    message = (
        "code-review-graph refreshed after a git working-tree change."
        if proc.returncode == 0
        else "WARNING: code-review-graph update FAILED after a git working-tree change. "
        "Graph reads are STALE until you run 'code-review-graph update' manually."
    )
    output = {
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": message,
        }
    }
    _telemetry(ctx, "post-bash-graph", output)
    return output


def post_bash_quiet(ctx: Any) -> dict[str, Any]:
    _source_project_env(ctx)
    return _body(ctx, "tooling/hooks/claude/_bash_quiet.py").hook_output(ctx.payload)


def context_telemetry(ctx: Any) -> None:
    telemetry = _conductor(ctx, "conductor.context_telemetry")
    path = _telemetry_path(telemetry)
    try:
        telemetry.record(telemetry.event(ctx.payload), path)
    except (OSError, TypeError, ValueError) as exc:
        print(f"context telemetry unavailable: {exc}", file=sys.stderr)


# ── SessionEnd ──────────────────────────────────────────────────────────


def obsidian_session_end(ctx: Any) -> None:
    _body(ctx, "tooling/hooks/claude/obsidian_sync.py").cmd_session_end()
