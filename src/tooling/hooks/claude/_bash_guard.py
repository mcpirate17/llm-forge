#!/usr/bin/env python3
"""Legacy command guard entrypoint backed by forge's native parser."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from native_legacy_bridge import call


def check(command: str) -> str | None:
    return call("guard-check", command=command)


def check_command(argv: list[str]) -> str | None:
    return call("guard-check-command", argv=argv)


def main() -> int:
    try:
        reason = check(sys.stdin.read())
    except RuntimeError as exc:
        print(f"bash guard unavailable: {exc}", file=sys.stderr)
        return 2
    if reason:
        print(reason)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
