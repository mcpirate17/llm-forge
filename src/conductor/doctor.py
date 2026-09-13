"""``conductor doctor --harness``: prove the settings that dominate agent cost.

The hook doctor (``tooling.hooks.dispatch.doctor``) proves declared hooks are
alive; this one proves the *values* around them are the cheap ones. It reads
the project's ``.claude/settings.json`` (under ``$CLAUDE_PROJECT_DIR``) and
the user's ``~/.claude/settings.json`` (under ``$HOME``; a missing user file
is a skip, not a failure -- the harness default applies) and checks four
items, PASS/FAIL each, with the repair spelled out as the single JSON edit
``--fix`` applies:

- ``subagentPromptCacheTtl`` is ``"1h"``: a five-minute TTL rewarms a
  ~180K-token prefix at write price for every subagent spawn.
- every hook event routes through the one dispatcher command
  (``registry.settings_block``'s wiring, or ``forge hook <Event>`` once that
  binary exists) -- the project file must wire all of the dispatcher's
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


def _dispatcher_forms(event: str) -> frozenset[str]:
    """The two commands that legitimately serve an event.

    ``$CLAUDE_PROJECT_DIR/.claude/hooks/dispatch.py <Event>`` today;
    ``forge hook <Event>`` once the Rust dispatcher carries the launcher.
    """
    return frozenset({registry.dispatcher_command(event), f"forge hook {event}"})


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


def _check_hooks(scope: str, payload: dict[str, Any], *, require_wiring: bool) -> Finding:
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
            if command not in _dispatcher_forms(event):
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


def _findings_for(scope: str, payload: dict[str, Any]) -> list[Finding]:
    # The dispatcher wiring is the project file's job (bootstrap writes it
    # there); the user file only has to not bypass it.
    return [
        _check_ttl(scope, payload),
        _check_hooks(scope, payload, require_wiring=scope == "project"),
        _check_limit(scope, payload),
        _check_model(scope, payload),
    ]


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
        raise ValueError(f"settings file {path} is not readable JSON: {exc}") from exc
    if not isinstance(payload, dict):
        raise ValueError(f"settings file {path} is not a JSON object")
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

    Each file is checked independently: the project file must exist (the
    dispatcher wiring lives there) -- its absence is a FAIL whose fix writes
    the canonical settings -- while the user file may legitimately be absent,
    which reports one SKIP row, not a FAIL. A file that exists but is not
    readable JSON raises, whichever file it is.
    """
    user_path = home / ".claude" / "settings.json"
    project_path = project / ".claude" / "settings.json"
    findings: list[Finding] = []
    if project_path.is_file():
        findings.extend(_findings_for("project", _load(project_path)))
        project_target = project_path
    else:
        findings.append(
            Finding(
                "project",
                CHECK_FILE,
                "FAIL",
                f"no project settings at {project_path}",
                "write the canonical dispatcher settings",
            )
        )
        project_target = None
    if user_path.is_file():
        findings.extend(_findings_for("user", _load(user_path)))
        user_target = user_path
    else:
        findings.append(
            Finding("user", CHECK_FILE, "SKIP", f"no user settings at {user_path}")
        )
        user_target = None
    return findings, project_target, user_target


def _fix_everything(
    findings: list[Finding], project: Path, project_path: Path | None, user_path: Path | None
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
    lines.append(f"harness-doctor | {'FAIL' if failed else 'PASS'} fails={failed}{suffix}")
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
            fixed = _fix_everything(findings, project, project_path, user_path)
            # Report the post-fix truth: what still fails after the edits,
            # so `--fix` exit 0 means the settings are now the cheap ones.
            findings, _, _ = diagnose(project, home)
    except ValueError as exc:
        print(f"harness-doctor | FAIL {exc}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps({"findings": [f.__dict__ for f in findings], "fixed": fixed}, indent=2))
    else:
        print(render(findings, fixed=fixed))
    return 1 if any(f.status == "FAIL" for f in findings) else 0


if __name__ == "__main__":
    raise SystemExit(main())
