"""Hook doctor: prove every hook declared in ``.claude/settings.json`` is alive.

``python -m tooling.hooks.dispatch.doctor [--project-dir DIR] [--settings FILE]``

For each declared command: the script exists and is non-empty, carries the
executable bit when invoked directly, has a resolvable shebang or interpreter,
compiles (Python) or resolves as a module (``python -m``), resolves to a
registered hook (legacy or dispatcher wiring), and runs on a synthetic payload
within its timeout, exiting 0 with either nothing or valid JSON on stdout. A
non-zero exit is DEAD even though the harness ignores it — that is the silent
failure this tool exists to catch. Exit status is 1 when any hook is DEAD.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from tooling.hooks.dispatch import registry
from tooling.hooks.dispatch.payloads import TOOLS_PER_EVENT, scoped_env, synthetic

INTERPRETERS = frozenset({"python", "python3", "bash", "sh"})


@dataclass
class Declared:
    event: str
    matcher: str
    command: str
    timeout: int


@dataclass
class Resolved:
    argv: list[str]
    env: dict[str, str]
    interpreter: str | None
    module: str | None
    script: Path | None


@dataclass
class Report:
    declared: Declared
    status: str = "OK"
    problems: list[str] = field(default_factory=list)
    elapsed_ms: float = 0.0
    decision: str = ""

    def dead(self, problem: str) -> None:
        self.status = "DEAD"
        self.problems.append(problem)

    def warn(self, problem: str) -> None:
        if self.status != "DEAD":
            self.status = "WARN"
        self.problems.append(problem)


def load_settings(path: Path) -> list[Declared]:
    data = json.loads(path.read_text(encoding="utf-8"))
    out: list[Declared] = []
    for event, groups in data.get("hooks", {}).items():
        for group in groups:
            for hook in group.get("hooks", []):
                if hook.get("type", "command") != "command":
                    continue
                out.append(
                    Declared(
                        event,
                        group.get("matcher", ""),
                        hook["command"],
                        int(hook.get("timeout", 60)),
                    )
                )
    return out


def resolve(command: str, project_dir: Path) -> Resolved:
    expanded = command.replace("${CLAUDE_PROJECT_DIR}", str(project_dir)).replace(
        "$CLAUDE_PROJECT_DIR", str(project_dir)
    )
    tokens = shlex.split(expanded)
    env: dict[str, str] = {}
    if tokens and tokens[0] == "env":
        tokens = tokens[1:]
        while tokens and "=" in tokens[0] and not tokens[0].startswith("/"):
            key, _, value = tokens[0].partition("=")
            env[key] = value
            tokens = tokens[1:]
    interpreter = module = None
    script: Path | None = None
    if tokens and Path(tokens[0]).name in INTERPRETERS:
        interpreter = tokens[0]
        rest = tokens[1:]
        if rest[:1] == ["-m"] and len(rest) > 1:
            module = rest[1]
        elif rest:
            script = Path(rest[0])
    elif tokens:
        script = Path(tokens[0])
    return Resolved(tokens, env, interpreter, module, script)


def _shebang(script: Path) -> str | None:
    with script.open("rb") as handle:
        first = handle.readline()
    if not first.startswith(b"#!"):
        return None
    return first[2:].decode("utf-8", "replace").strip()


def static_checks(resolved: Resolved, report: Report, project_dir: Path) -> None:
    script = resolved.script
    if resolved.module:
        probe = subprocess.run(
            [resolved.interpreter or "python3", "-c", f"import {resolved.module}"],
            capture_output=True,
            text=True,
            cwd=project_dir,
            env={**os.environ, "PYTHONPATH": str(project_dir)},
            timeout=30,
            check=False,
        )
        if probe.returncode != 0:
            report.dead(
                f"module {resolved.module} does not import: {probe.stderr.strip()[-200:]}"
            )
        return
    if script is None:
        report.dead("no script or module in command")
        return
    if not script.is_file():
        report.dead(f"missing: {script}")
        return
    if script.stat().st_size == 0:
        report.dead(f"empty file: {script}")
        return
    if resolved.interpreter is None:
        if not script.stat().st_mode & stat.S_IXUSR:
            report.dead(
                f"not executable (mode {oct(script.stat().st_mode)[-3:]}): {script}"
            )
        shebang = _shebang(script)
        if shebang is None:
            report.dead(f"no shebang: {script}")
        else:
            words = shebang.split()
            target = (
                words[1] if words[0].endswith("/env") and len(words) > 1 else words[0]
            )
            if shutil.which(target) is None and not Path(target).is_file():
                report.dead(f"shebang interpreter not resolvable: {shebang}")
    elif shutil.which(resolved.interpreter) is None:
        report.dead(f"interpreter not on PATH: {resolved.interpreter}")
    if script.suffix == ".py":
        try:
            compile(script.read_bytes(), str(script), "exec")
        except SyntaxError as exc:
            report.dead(f"does not compile: {exc}")


def registry_check(declared: Declared, report: Report) -> None:
    if registry.resolve_legacy(declared.command) is not None:
        return
    if registry.resolve_dispatcher(declared.command) is not None:
        return
    report.dead("command resolves to no registered hook (registry.py)")


def _tool_for(declared: Declared) -> str:
    for tool in TOOLS_PER_EVENT.get(declared.event, ()):
        if registry.HookSpec("probe", declared.event, declared.matcher, 1, "").matches(
            tool
        ):
            return tool
    raise ValueError(
        f"no synthetic tool matches {declared.matcher!r} for {declared.event}"
    )


def run_check(
    declared: Declared, report: Report, project_dir: Path, scratch: Path
) -> dict[str, Any] | None:
    payload = synthetic(declared.event, _tool_for(declared), project_dir, scratch)
    env = scoped_env(project_dir, scratch)
    started = time.perf_counter()
    try:
        proc = subprocess.run(
            ["bash", "-c", declared.command],
            input=json.dumps(payload),
            capture_output=True,
            text=True,
            cwd=project_dir,
            env=env,
            timeout=declared.timeout,
            check=False,
        )
    except subprocess.TimeoutExpired:
        report.elapsed_ms = (time.perf_counter() - started) * 1000
        report.dead(f"timed out after {declared.timeout}s")
        return None
    report.elapsed_ms = (time.perf_counter() - started) * 1000
    if proc.returncode != 0:
        report.dead(
            f"exit {proc.returncode}; stderr: {proc.stderr.strip()[-200:] or '<empty>'}"
        )
        return None
    if not proc.stdout.strip():
        spec = registry.resolve_legacy(declared.command)
        if spec is None or spec.emits:
            report.warn("wrote nothing to stdout")
        return None
    try:
        out = json.loads(proc.stdout)
    except ValueError:
        report.dead(f"stdout is not JSON: {proc.stdout.strip()[:120]!r}")
        return None
    if not isinstance(out, dict):
        report.dead("stdout JSON is not an object")
        return None
    specific = out.get("hookSpecificOutput")
    if isinstance(specific, dict):
        name = specific.get("hookEventName")
        if name and name != declared.event:
            report.warn(f"hookEventName {name!r} != {declared.event}")
        report.decision = str(specific.get("permissionDecision") or "")
    return out


def diagnose(
    declared: list[Declared], project_dir: Path, scratch: Path
) -> list[Report]:
    reports: list[Report] = []
    for item in declared:
        report = Report(item)
        registry_check(item, report)
        static_checks(resolve(item.command, project_dir), report, project_dir)
        if report.status != "DEAD" or "not executable" in " ".join(report.problems):
            run_check(item, report, project_dir, scratch)
        reports.append(report)
    return reports


def render(reports: list[Report]) -> str:
    rows = [("STATUS", "EVENT", "MATCHER", "MS", "DECISION", "COMMAND / PROBLEMS")]
    for r in reports:
        rows.append(
            (
                r.status,
                r.declared.event,
                r.declared.matcher or "(all)",
                f"{r.elapsed_ms:.0f}",
                r.decision or "-",
                r.declared.command.replace("$CLAUDE_PROJECT_DIR/", ""),
            )
        )
        for problem in r.problems:
            rows.append(("", "", "", "", "", f"  !! {problem}"))
    widths = [max(len(row[i]) for row in rows) for i in range(5)]
    lines = []
    for row in rows:
        head = "  ".join(cell.ljust(widths[i]) for i, cell in enumerate(row[:5]))
        lines.append(f"{head}  {row[5]}".rstrip())
    dead = sum(r.status == "DEAD" for r in reports)
    warn = sum(r.status == "WARN" for r in reports)
    lines.append(
        f"hook-doctor | {'FAIL' if dead else 'PASS'} dead={dead} warn={warn} total={len(reports)}"
    )
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project-dir", type=Path, default=Path.cwd())
    parser.add_argument("--settings", type=Path, default=None)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    project_dir = args.project_dir.resolve()
    settings = args.settings or project_dir / ".claude" / "settings.json"
    if not settings.is_file():
        print(f"hook-doctor | FAIL no settings file at {settings}", file=sys.stderr)
        return 1
    with tempfile.TemporaryDirectory(prefix="hook-doctor-") as tmp:
        reports = diagnose(load_settings(settings), project_dir, Path(tmp))
    if args.json:
        print(
            json.dumps(
                [r.__dict__ | {"declared": r.declared.__dict__} for r in reports],
                indent=2,
            )
        )
    else:
        print(render(reports))
    return 1 if any(r.status == "DEAD" for r in reports) else 0


if __name__ == "__main__":
    raise SystemExit(main())
