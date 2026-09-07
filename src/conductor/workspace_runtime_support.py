"""Implementation helpers for workspace hook verification."""

from __future__ import annotations

import ast
import json
import os
from pathlib import Path
import subprocess
import sys
from typing import Any, Callable


HookCall = Callable[[dict[str, Any], Path], subprocess.CompletedProcess[str]]
RunCommand = Callable[..., subprocess.CompletedProcess[str]]


def check_hook_sources(
    root: Path,
    failures: list[str],
    evidence: dict[str, Any],
    *,
    hook_call: HookCall,
    run_command: RunCommand,
) -> None:
    """Validate each native raw guard and every referenced hook program."""
    scripts = {
        "codex": root / ".codex" / "hooks" / "pre-edit.sh",
        "claude": root / ".claude" / "hooks" / "pre-edit.sh",
        "qwen": root / ".qwen" / "hooks" / "pre-edit.sh",
        "grok": root / ".grok" / "hooks" / "pre-edit.sh",
    }
    payloads = {
        "codex": {"tool_name": "Read", "tool_input": {"file_path": ".current_work.md"}},
        "claude": {
            "tool_name": "Read",
            "tool_input": {"file_path": ".current_work.md"},
        },
        "qwen": {
            "tool_name": "read_file",
            "tool_input": {"file_path": ".current_work.md"},
        },
        "grok": {
            "hookEventName": "pre_tool_use",
            "toolName": "read_file",
            "toolInput": {"target_file": ".current_work.md"},
        },
    }
    for name, program in scripts.items():
        result = hook_call(payloads[name], program)
        denied = "BLOCKED:" in result.stdout
        evidence[f"{name}-native-raw-deny"] = {
            "returncode": result.returncode,
            "denied": denied,
        }
        if result.returncode != 0 or not denied:
            failures.append(f"{name}-native-raw-deny")
    shell_scripts = sorted(
        {
            *root.glob(".codex/hooks/*.sh"),
            *root.glob(".claude/hooks/*.sh"),
            *root.glob(".qwen/hooks/*.sh"),
            *root.glob(".grok/hooks/*.sh"),
        }
    )
    shell_syntax: dict[str, int] = {}
    for path in shell_scripts:
        result = run_command(["bash", "-n", str(path)], timeout=10)
        relative = path.relative_to(root).as_posix()
        shell_syntax[relative] = result.returncode
        if result.returncode != 0:
            failures.append(f"syntax:{relative}")
    evidence["shell_syntax"] = shell_syntax
    python_scripts = sorted(
        {
            root / ".agent_hooks" / "crg_gate.py",
            *root.glob(".codex/hooks/*.py"),
            *root.glob(".claude/hooks/*.py"),
        }
    )
    python_syntax: dict[str, bool] = {}
    for path in python_scripts:
        relative = path.relative_to(root).as_posix()
        try:
            ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        except (OSError, UnicodeError, SyntaxError):
            python_syntax[relative] = False
            failures.append(f"syntax:{relative}")
        else:
            python_syntax[relative] = True
    evidence["python_syntax"] = python_syntax


def check_hook_noops(
    root: Path,
    failures: list[str],
    evidence: dict[str, Any],
    *,
    hook_call: HookCall,
) -> None:
    """Exercise destructive-command denials and bounded post-hook no-ops."""
    controls = {
        "codex-pre-bash-deny": (
            root / ".codex" / "hooks" / "pre-bash.sh",
            {"tool_input": {"command": "git reset --hard"}},
            "BLOCKED:",
        ),
        "claude-pre-bash-deny": (
            root / ".claude" / "hooks" / "pre-bash.sh",
            {"tool_input": {"command": "git reset --hard"}},
            "BLOCKED:",
        ),
        "codex-post-edit-noop": (
            root / ".codex" / "hooks" / "post-edit.sh",
            {"tool_input": {"file_path": "/nonexistent/workspace-probe"}},
            "PostToolUse",
        ),
        "claude-post-edit-noop": (
            root / ".claude" / "hooks" / "post-edit.sh",
            {"tool_input": {"file_path": "/nonexistent/workspace-probe"}},
            "PostToolUse",
        ),
        "claude-post-bash-noop": (
            root / ".claude" / "hooks" / "post-bash-graph.sh",
            {"tool_input": {"command": "git status --short"}},
            "PostToolUse",
        ),
    }
    for name, (program, payload, marker) in controls.items():
        result = hook_call(payload, program)
        ok = result.returncode == 0 and marker in result.stdout
        evidence[name] = {"returncode": result.returncode, "passed": ok}
        if not ok:
            failures.append(name)
    for launcher in ("codex", "claude"):
        program = root / f".{launcher}" / "hooks" / "obsidian_sync.py"
        result = subprocess.run(
            [str(program), "post-edit"],
            input="{}",
            text=True,
            capture_output=True,
            timeout=5,
            check=False,
        )
        ok = result.returncode == 0 and "PostToolUse" in result.stdout
        evidence[f"{launcher}-obsidian-noop"] = {
            "returncode": result.returncode,
            "passed": ok,
        }
        if not ok:
            failures.append(f"{launcher}-obsidian-noop")


def _preamble_env() -> dict[str, str]:
    """Import the package that supplied this runtime check from a foreign cwd."""
    environment = os.environ.copy()
    package_root = str(Path(__file__).resolve().parents[1])
    inherited = environment.get("PYTHONPATH")
    environment["PYTHONPATH"] = (
        os.pathsep.join((package_root, inherited)) if inherited else package_root
    )
    return environment


def _is_session_start_payload(output: str) -> bool:
    """Validate the hook envelope without requiring a project policy."""
    try:
        payload = json.loads(output)
    except json.JSONDecodeError:
        return False
    hook_output = (
        payload.get("hookSpecificOutput") if isinstance(payload, dict) else None
    )
    return (
        isinstance(hook_output, dict)
        and hook_output.get("hookEventName") == "SessionStart"
        and isinstance(hook_output.get("additionalContext"), str)
    )


def check_preamble_and_grok(
    root: Path,
    failures: list[str],
    evidence: dict[str, Any],
    *,
    run_command: RunCommand,
    sha256_bytes: Callable[[bytes], str],
    grok_argv: Callable[[], list[str]],
) -> None:
    """Validate the injected preamble and Grok's trusted project hook discovery."""
    root = root.resolve()
    preamble = run_command(
        [
            sys.executable,
            "-P",
            "-m",
            "conductor.session_preamble",
            "hook",
            "--state",
            str(root / "conductor" / "active_state.json"),
            "--repo",
            str(root),
        ],
        cwd=root,
        env=_preamble_env(),
        timeout=30,
    )
    hook_payload_valid = _is_session_start_payload(preamble.stdout)
    preamble_ok = preamble.returncode == 0 and hook_payload_valid
    evidence["session-preamble"] = {
        "returncode": preamble.returncode,
        "hook_payload_valid": hook_payload_valid,
        "passed": preamble_ok,
        "stdout_sha256": sha256_bytes(preamble.stdout.encode()),
    }
    if not preamble_ok:
        failures.append("session-preamble")
    grok_inspect = run_command(grok_argv(), timeout=30)
    try:
        inspect_payload = json.loads(grok_inspect.stdout)
    except json.JSONDecodeError:
        inspect_payload = {}
    hooks = inspect_payload.get("hooks")
    matchers = {
        str(hook.get("matcher"))
        for hook in (hooks if isinstance(hooks, list) else [])
        if isinstance(hook, dict)
        and isinstance(hook.get("source"), dict)
        and hook["source"].get("path") == str(root / ".grok" / "hooks")
        and hook.get("event") == "pre_tool_use"
    }
    ok = (
        grok_inspect.returncode == 0
        and isinstance(inspect_payload.get("projectRoot"), str)
        and Path(inspect_payload["projectRoot"]).resolve() == root
        and inspect_payload.get("projectTrusted") is True
        and "Read|read|read_file" in matchers
        and "Bash|run_shell_command|shell" in matchers
    )
    evidence["grok-inspect"] = {
        "returncode": grok_inspect.returncode,
        "project_trusted": inspect_payload.get("projectTrusted"),
        "matchers": sorted(matchers),
        "passed": ok,
    }
    if not ok:
        failures.append("grok-inspect")
