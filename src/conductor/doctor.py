"""``conductor doctor --harness``: prove the settings that dominate agent cost.

The hook doctor (``tooling.hooks.dispatch.doctor``) proves declared hooks are
alive; this one proves the *values* around them are the cheap ones. It reads
the project's ``.claude/settings.json`` (under ``$CLAUDE_PROJECT_DIR``) and
the user's ``~/.claude/settings.json`` (under ``$HOME``; a missing user file
is a skip, not a failure -- the harness default applies) and checks four
items after applying project-over-user scalar/environment precedence. Hooks
remain additive across the two scopes. Repairs target the effective value's
owning scope, with the repair spelled out as the single JSON edit
``--fix`` applies:

- ``subagentPromptCacheTtl`` is ``"1h"``: a five-minute TTL rewarms a
  ~180K-token prefix at write price for every subagent spawn.
- every hook event routes through the one dispatcher command
  (``registry.settings_block``'s wiring, or ``forge hook <Event>`` once that
  binary exists) -- the effective settings must wire all of the dispatcher's
  events, and neither file may carry a stray per-hook command under them,
  because a stray command is a hook the doctor cannot vouch for and the
  dispatcher cannot bound.
- ``BASH_QUIET_LIMIT_BYTES`` is set in the settings ``env`` block: the bound
  ``_bash_quiet`` enforces exists either way (module default 8000), but
  policy belongs where everyone reads it, not in a source-file literal.
- the shared ``model`` key names a known Claude model id (a ``[...]``
  thinking-budget suffix is fine) -- a ``glm-*`` or other foreign id left
  there sends every session to a model this platform's hooks were never
  tested against.

Malformed JSON fails loud (exit 2), never silently skipped. Exit is 1 when
any check FAILs, 0 when all pass; ``--fix`` applies exactly the failed
items' edits (idempotent: a second run finds nothing to do).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final

from tooling.hooks.dispatch import registry


class SettingsSchemaError(ValueError):
    """Serialized harness settings are unreadable or violate their schema."""


TTL_KEY: Final[str] = "subagentPromptCacheTtl"
TTL_WANTED: Final[str] = "1h"
LIMIT_ENV_KEY: Final[str] = "BASH_QUIET_LIMIT_BYTES"
LIMIT_DEFAULT: Final[str] = "8000"
MODEL_KEY: Final[str] = "model"
CHECK_HOOKS: Final[str] = "hooks-dispatcher"
CHECK_FILE: Final[str] = "(file)"
# The fleet this platform is tested against; a settings ``model`` outside this
# set fails the doctor on purpose. Extend the set when the fleet changes --
# that is the review, not an inconvenience.
KNOWN_MODELS: Final[frozenset[str]] = frozenset(
    {
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-fable-5-1",
        "claude-haiku-4-5-20251001",
        "opus",
        "sonnet",
        "haiku",
        "fable",
        "default",
    }
)


@dataclass
class Finding:
    """One check on one settings file; ``fix`` names the edit ``--fix`` makes."""

    scope: str
    check: str
    status: str  # PASS | FAIL | SKIP
    detail: str
    fix: str = ""


def _check_ttl(scope: str, payload: dict[str, Any]) -> Finding:
    value = payload.get(TTL_KEY)
    if value == TTL_WANTED:
        return Finding(scope, TTL_KEY, "PASS", f"{TTL_KEY} is {value!r}")
    return Finding(
        scope,
        TTL_KEY,
        "FAIL",
        f"{TTL_KEY} is {value!r}, not {TTL_WANTED!r} -- a short TTL rewarms the "
        "session prefix at write price for every subagent",
        f"set {TTL_KEY} to {TTL_WANTED!r}",
    )


def _declared_commands(payload: dict[str, Any]) -> dict[str, list[str]]:
    hooks = payload.get("hooks")
    declared: dict[str, list[str]] = {}
    if not isinstance(hooks, dict):
        return declared
    for event, groups in hooks.items():
        commands: list[str] = []
        for group in groups if isinstance(groups, list) else []:
            for hook in group.get("hooks", []):
                if hook.get("type", "command") == "command":
                    commands.append(str(hook.get("command", "")))
        declared[str(event)] = commands
    return declared


def _check_hooks(
    scope: str, payload: dict[str, Any], *, require_wiring: bool
) -> Finding:
    declared = _declared_commands(payload)
    problems: list[str] = []
    wired = 0
    for event in registry.EVENTS:
        commands = declared.get(event, [])
        if not commands:
            if require_wiring:
                problems.append(f"{event}: no dispatcher wiring")
            continue
        wired += 1
        for command in commands:
            if registry.resolve_dispatcher(command) != event:
                problems.append(f"{event}: {command!r} bypasses the dispatcher")
    if problems:
        return Finding(
            scope,
            CHECK_HOOKS,
            "FAIL",
            "; ".join(problems),
            "rewrite the hooks block to registry.settings_block() "
            "(every event through the single dispatcher)",
        )
    if wired:
        return Finding(scope, CHECK_HOOKS, "PASS", "every wired event dispatches")
    return Finding(
        scope,
        CHECK_HOOKS,
        "PASS",
        "no dispatcher-event hooks declared (wiring lives in project settings)",
    )


def _check_limit(scope: str, payload: dict[str, Any]) -> Finding:
    env = payload.get("env")
    value = env.get(LIMIT_ENV_KEY) if isinstance(env, dict) else None
    if isinstance(value, str) and value.isdigit() and int(value) > 0:
        return Finding(scope, LIMIT_ENV_KEY, "PASS", f"env {LIMIT_ENV_KEY}={value!r}")
    if value is None:
        detail = (
            f"env {LIMIT_ENV_KEY} unset -- the bound lives in a source-file "
            "default instead of settings policy"
        )
    else:
        detail = f"env {LIMIT_ENV_KEY} is {value!r}, not a positive integer"
    return Finding(
        scope,
        LIMIT_ENV_KEY,
        "FAIL",
        detail,
        f"set env {LIMIT_ENV_KEY} to {LIMIT_DEFAULT!r}",
    )


def _known_model(value: str) -> bool:
    """Accept a known id, optionally with a ``[...]`` thinking-budget suffix."""
    base = value.split("[", 1)[0]
    if base not in KNOWN_MODELS:
        return False
    return value == base or (value.endswith("]") and "[" in value)


def _check_model(scope: str, payload: dict[str, Any]) -> Finding:
    value = payload.get(MODEL_KEY)
    if value is None:
        return Finding(scope, MODEL_KEY, "PASS", "no model override")
    if isinstance(value, str) and _known_model(value):
        return Finding(scope, MODEL_KEY, "PASS", f"model is {value!r}")
    return Finding(
        scope,
        MODEL_KEY,
        "FAIL",
        f"model is {value!r}, not a known Claude model id",
        "remove the model key so the harness default applies",
    )


def _apply_fixes(payload: dict[str, Any], findings: list[Finding]) -> bool:
    """Apply the failed items' edits in place; return whether anything moved."""
    touched = False
    for finding in findings:
        if finding.status != "FAIL":
            continue
        if finding.check == TTL_KEY:
            payload[TTL_KEY] = TTL_WANTED
        elif finding.check == CHECK_HOOKS:
            payload["hooks"] = registry.settings_block()["hooks"]
        elif finding.check == LIMIT_ENV_KEY:
            payload.setdefault("env", {})[LIMIT_ENV_KEY] = LIMIT_DEFAULT
        elif finding.check == MODEL_KEY:
            payload.pop(MODEL_KEY, None)
        else:
            raise ValueError(f"no fix known for check {finding.check!r}")
        touched = True
    return touched


def _load(path: Path) -> dict[str, Any]:
    """Parse one settings file, failing loud on anything that is not JSON."""
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise SettingsSchemaError(
            f"settings file {path} is not readable JSON: {exc}"
        ) from exc
    if not isinstance(payload, dict):
        raise SettingsSchemaError(f"settings file {path} is not a JSON object")
    return payload


def _save(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def _canonical_settings() -> dict[str, Any]:
    """What ``--fix`` writes for a missing project file: the dispatcher block."""
    payload: dict[str, Any] = dict(registry.settings_block())
    payload["env"] = {LIMIT_ENV_KEY: LIMIT_DEFAULT}
    payload[TTL_KEY] = TTL_WANTED
    return payload


def diagnose(
    project: Path, home: Path
) -> tuple[list[Finding], Path | None, Path | None]:
    """Collect findings for both settings files, plus the paths ``--fix`` writes.

    Project scalars override user scalars; environment keys merge by key and
    hooks from both scopes remain active. Repairs target the winning scope.
    """
    user_path = home / ".claude" / "settings.json"
    project_path = project / ".claude" / "settings.json"
    findings: list[Finding] = []
    project_target = project_path if project_path.is_file() else None
    user_target = user_path if user_path.is_file() else None
    project_settings = _load(project_path) if project_target else {}
    user_settings = _load(user_path) if user_target else {}
    if project_target is None:
        findings.append(
            Finding(
                "project",
                CHECK_FILE,
                "FAIL",
                f"no project settings at {project_path}",
                "write the canonical dispatcher settings",
            )
        )
    if user_target is None:
        findings.append(
            Finding("user", CHECK_FILE, "SKIP", f"no user settings at {user_path}")
        )
    effective = {**user_settings, **project_settings}
    user_env = user_settings.get("env", {})
    project_env = project_settings.get("env", {})
    if not isinstance(user_env, dict) or not isinstance(project_env, dict):
        raise SettingsSchemaError("settings env must be a JSON object")
    effective["env"] = {**user_env, **project_env}
    for key, check in ((TTL_KEY, _check_ttl), (MODEL_KEY, _check_model)):
        scope = (
            "project" if key in project_settings or key not in user_settings else "user"
        )
        findings.append(check(scope, effective))
    limit_scope = (
        "project"
        if LIMIT_ENV_KEY in project_env or LIMIT_ENV_KEY not in user_env
        else "user"
    )
    findings.append(_check_limit(limit_scope, effective))
    for scope, payload in (("project", project_settings), ("user", user_settings)):
        if payload:
            findings.append(_check_hooks(scope, payload, require_wiring=False))
    wired = set(_declared_commands(project_settings)) | set(
        _declared_commands(user_settings)
    )
    missing = [
        event
        for event in registry.EVENTS
        if event not in wired
        or not (
            _declared_commands(project_settings).get(event)
            or _declared_commands(user_settings).get(event)
        )
    ]
    if missing and project_target is not None:
        findings.append(
            Finding(
                "project",
                CHECK_HOOKS,
                "FAIL",
                f"no effective dispatcher wiring for {', '.join(missing)}",
                "write missing dispatcher events in project settings",
            )
        )
    return findings, project_target, user_target


def _fix_everything(
    findings: list[Finding],
    project: Path,
    project_path: Path | None,
    user_path: Path | None,
) -> int:
    """Apply failed items' edits to the files on disk; return the edit count."""
    fixed = 0
    for scope, path in (("project", project_path), ("user", user_path)):
        if path is None:
            continue
        scoped = [f for f in findings if f.scope == scope]
        payload = _load(path)
        if _apply_fixes(payload, scoped):
            _save(path, payload)
            fixed += sum(f.status == "FAIL" for f in scoped)
    if project_path is None:
        _save(project / ".claude" / "settings.json", _canonical_settings())
        fixed += 1
    return fixed


def render(findings: list[Finding], *, fixed: int) -> str:
    lines = []
    for f in findings:
        row = f"{f.scope} | {f.check} | {f.status} | {f.detail}"
        if f.status == "FAIL":
            row += f" [fix: {f.fix}]"
        lines.append(row)
    failed = sum(f.status == "FAIL" for f in findings)
    suffix = f" fixed={fixed}" if fixed else ""
    lines.append(
        f"harness-doctor | {'FAIL' if failed else 'PASS'} fails={failed}{suffix}"
    )
    return "\n".join(lines)


def _resolve_roots(args: argparse.Namespace) -> tuple[Path, Path] | None:
    project = args.project_dir or os.environ.get("CLAUDE_PROJECT_DIR") or os.getcwd()
    home = args.home or os.environ.get("HOME") or ""
    if not home:
        print("conductor doctor: no --home and $HOME is unset", file=sys.stderr)
        return None
    return Path(project), Path(home)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="conductor doctor", description="prove host harness settings"
    )
    parser.add_argument("--harness", action="store_true", help="check .claude settings")
    parser.add_argument(
        "--fix", action="store_true", help="apply the failed items' edits"
    )
    parser.add_argument("--project-dir", type=Path, default=None)
    parser.add_argument("--home", type=Path, default=None)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if not args.harness:
        print(
            "conductor doctor: choose a mode (only --harness exists today)",
            file=sys.stderr,
        )
        return 2
    roots = _resolve_roots(args)
    if roots is None:
        return 2
    project, home = roots
    try:
        findings, project_path, user_path = diagnose(project, home)
        fixed = 0
        if args.fix:
            # Project model settings take precedence over user settings.
            # Validate the effective value again after each scope is repaired.
            for _ in range(2):
                fixed += _fix_everything(findings, project, project_path, user_path)
                findings, project_path, user_path = diagnose(project, home)
                if not any(f.status == "FAIL" for f in findings):
                    break
    except ValueError as exc:
        print(f"harness-doctor | FAIL {exc}", file=sys.stderr)
        return 2
    if args.json:
        print(
            json.dumps(
                {"findings": [f.__dict__ for f in findings], "fixed": fixed}, indent=2
            )
        )
    else:
        print(render(findings, fixed=fixed))
    return 1 if any(f.status == "FAIL" for f in findings) else 0


if __name__ == "__main__":
    raise SystemExit(main())
