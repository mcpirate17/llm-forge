"""Latency probe: wall time per hook event for whatever settings.json declares.

``python -m tooling.hooks.dispatch.bench [--project-dir DIR] [--settings FILE]
[--iterations N]`` runs every declared command matching each (event, tool)
probe through ``bash -c`` exactly as the harness does and reports, per probe,
the sequential sum of the matching commands (the CPU the event costs) and the
slowest single command (the wall the harness sees when it runs them in
parallel). Median over N iterations; synthetic, side-effect-bounded payloads.
"""

from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

from tooling.hooks.dispatch import registry
from tooling.hooks.dispatch.doctor import Declared, load_settings
from tooling.hooks.dispatch.payloads import scoped_env, synthetic

PROBES: tuple[tuple[str, str], ...] = (
    ("PreToolUse", "Bash"),
    ("PostToolUse", "Bash"),
    ("PreToolUse", "Read"),
    ("PostToolUse", "Read"),
    ("PreToolUse", "Edit"),
    ("PostToolUse", "Edit"),
)


def _matching(declared: list[Declared], event: str, tool: str) -> list[Declared]:
    return [
        d
        for d in declared
        if d.event == event
        and registry.HookSpec("p", event, d.matcher, 1, "").matches(tool)
    ]


def _time_one(
    command: str, payload: str, env: dict[str, str], cwd: Path, timeout: int
) -> float:
    started = time.perf_counter()
    subprocess.run(
        ["bash", "-c", command],
        input=payload,
        capture_output=True,
        text=True,
        cwd=cwd,
        env=env,
        timeout=timeout,
        check=False,
    )
    return (time.perf_counter() - started) * 1000


def bench(
    declared: list[Declared], project_dir: Path, scratch: Path, iterations: int
) -> list[dict[str, float | str | int]]:
    env = scoped_env(project_dir, scratch)
    rows: list[dict[str, float | str | int]] = []
    for event, tool in PROBES:
        commands = _matching(declared, event, tool)
        payload = json.dumps(synthetic(event, tool, project_dir, scratch))
        sums: list[float] = []
        maxes: list[float] = []
        for _ in range(iterations):
            times = [
                _time_one(d.command, payload, env, project_dir, d.timeout)
                for d in commands
            ]
            sums.append(sum(times))
            maxes.append(max(times) if times else 0.0)
        rows.append(
            {
                "event": event,
                "tool": tool,
                "commands": len(commands),
                "sum_ms": round(statistics.median(sums), 1),
                "max_ms": round(statistics.median(maxes), 1),
            }
        )
    return rows


def render(rows: list[dict[str, float | str | int]], iterations: int) -> str:
    lines = [
        f"| event | tool | commands | sum ms (median of {iterations}) | slowest ms |",
        "|---|---|---|---|---|",
    ]
    for r in rows:
        lines.append(
            f"| {r['event']} | {r['tool']} | {r['commands']} | {r['sum_ms']} | {r['max_ms']} |"
        )
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project-dir", type=Path, default=Path.cwd())
    parser.add_argument("--settings", type=Path, default=None)
    parser.add_argument("--iterations", type=int, default=20)
    args = parser.parse_args(argv)
    project_dir = args.project_dir.resolve()
    settings = args.settings or project_dir / ".claude" / "settings.json"
    with tempfile.TemporaryDirectory(prefix="hook-bench-") as tmp:
        rows = bench(load_settings(settings), project_dir, Path(tmp), args.iterations)
    print(render(rows, args.iterations))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
