"""Provider capabilities and bootstrap config, using the installed adapters.

Only Claude and Codex have a dispatcher protocol. Qwen and Grok receive the
bounded messaging hook; installing it does not imply tool-guard coverage.
"""

from __future__ import annotations

import json
import shlex
import subprocess
from pathlib import Path
from typing import Any

from conductor import hook_installer
from tooling.hooks.dispatch.registry import EVENTS, settings_block

CAPABILITIES = {
    "claude": "dispatcher guards, context bounds, lifecycle hooks (native when available)",
    "codex": "dispatcher guards and lifecycle hooks; bounded A2A startup; Codex output adapter",
    "qwen": "bounded A2A SessionStart only; tool guards are not provisioned",
    "grok": "bounded A2A first UserPromptSubmit only; tool guards are not provisioned",
}


def provider_hooks(provider: str, python: Path, root: Path) -> dict[str, Any]:
    """Desired portable hooks; all interpreter and root paths are explicit."""
    spec = hook_installer.PROVIDERS[provider]
    payload: dict[str, Any] = {}
    if provider == "codex":
        payload = settings_block()
        for event in EVENTS:
            payload["hooks"][event][0]["hooks"][0]["command"] = shlex.join(
                [
                    str(python),
                    "-m",
                    "tooling.hooks.dispatch",
                    event,
                    "--project-dir",
                    str(root),
                    "--protocol",
                    "codex",
                ]
            )
    command = hook_installer.startup_command(spec, interpreter=str(python))
    return hook_installer.merge_install(payload, spec, command)["hooks"]


def check_provider(provider: str, python: Path, root: Path) -> list[str]:
    """Validate selected adapter installation without launching messaging services."""
    spec = hook_installer.PROVIDERS[provider]
    path = root / spec.relative_path
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, ValueError) as exc:
        return [f"{path}: cannot read provider settings: {exc}"]
    if not isinstance(payload, dict) or not isinstance(payload.get("hooks"), dict):
        return [f"{path}: hooks must be a JSON object"]
    wanted = provider_hooks(provider, python, root)
    errors = []
    for event, groups in wanted.items():
        actual = payload["hooks"].get(event, [])
        if not isinstance(actual, list) or any(group not in actual for group in groups):
            errors.append(f"{path}: {event} differs from configured {provider} adapter")
    # Imports prove the selected interpreter carries the installed adapter. Do
    # not invoke a2a_session_start: a read-only check must not spawn an endpoint.
    module = (
        "tooling.hooks.dispatch"
        if provider == "codex"
        else "conductor.a2a_session_start"
    )
    try:
        result = subprocess.run(
            [str(python), "-c", f"import {module}; import conductor_native"],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        errors.append(f"{provider}: configured interpreter unavailable: {exc}")
    else:
        if result.returncode:
            errors.append(
                f"{provider}: adapter import failed: {result.stderr.strip()[-500:]}"
            )
    return errors
