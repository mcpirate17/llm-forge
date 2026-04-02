#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
_COMMANDS: tuple[tuple[str, list[str], Path], ...] = (
    (
        "bench_e2e_quick",
        [
            sys.executable,
            "-m",
            "research.runtime.native.bench.bench_e2e",
            "--quick",
        ],
        ROOT,
    ),
    (
        "perf_summary_recent",
        [
            sys.executable,
            "-m",
            "research.tools.perf_summary",
            "--json",
            "--limit",
            "20",
        ],
        ROOT,
    ),
)


def _run(name: str, cmd: list[str], cwd: Path) -> dict:
    started = time.perf_counter()
    proc = subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        check=False,
    )
    return {
        "name": name,
        "command": cmd,
        "cwd": str(cwd),
        "returncode": proc.returncode,
        "elapsed_s": round(time.perf_counter() - started, 6),
        "stdout": proc.stdout,
        "stderr": proc.stderr,
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run lightweight profiling/benchmark hooks for CI"
    )
    parser.add_argument("--json-out", type=str, default="")
    parser.add_argument(
        "--fail-fast", action="store_true", help="Stop on first failure"
    )
    args = parser.parse_args()

    results = []
    for name, cmd, cwd in _COMMANDS:
        result = _run(name, cmd, cwd)
        results.append(result)
        if args.fail_fast and result["returncode"] != 0:
            break

    failed = [r["name"] for r in results if r["returncode"] != 0]
    payload = {
        "results": results,
        "failed_commands": failed,
        "ok": not failed,
    }

    if args.json_out:
        out = ROOT / args.json_out
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    else:
        print(json.dumps(payload, indent=2))

    if failed:
        print(
            f"\n{len(failed)} command(s) failed.",
            file=sys.stderr,
        )
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
