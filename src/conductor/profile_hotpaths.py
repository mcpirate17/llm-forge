#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def _run(cmd: list[str], cwd: Path) -> dict:
    proc = subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        check=False,
    )
    return {
        "command": cmd,
        "cwd": str(cwd),
        "returncode": proc.returncode,
        "stdout": proc.stdout,
        "stderr": proc.stderr,
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run lightweight profiling/benchmark hooks for CI"
    )
    parser.add_argument("--json-out", type=str, default="")
    args = parser.parse_args()

    results = []
    results.append(
        _run(
            [sys.executable, "runtime/native/bench/bench_e2e.py", "--quick"],
            ROOT / "research",
        )
    )
    results.append(
        _run(
            [
                sys.executable,
                "-m",
                "research.tools.perf_summary",
                "--json",
                "--limit",
                "20",
            ],
            ROOT,
        )
    )
    payload = {
        "results": results,
        "failed_commands": [r["command"] for r in results if r["returncode"] != 0],
    }

    if args.json_out:
        out = ROOT / args.json_out
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    else:
        print(json.dumps(payload, indent=2))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
