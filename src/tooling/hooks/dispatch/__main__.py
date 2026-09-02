"""``python -m tooling.hooks.dispatch <Event>``: run one hook event end to end.

Reads the hook payload on stdin, dispatches every registered hook for the
event, prints the merged JSON and exits 0. ``HOOK_DISPATCH_TRACE=1`` prints a
per-hook timing line to stderr. ``python -m tooling.hooks.dispatch settings``
prints the ``.claude/settings.json`` hooks block that wires every event here.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

from tooling.hooks.dispatch import runner
from tooling.hooks.dispatch.registry import EVENTS, settings_block


def _root(explicit: Path | None) -> Path:
    if explicit is not None:
        return explicit.resolve()
    for var in ("PROJECT_DIR", "CLAUDE_PROJECT_DIR"):
        value = os.environ.get(var, "").strip()
        if value:
            return Path(value).resolve()
    return Path(__file__).resolve().parents[3]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("event", choices=(*EVENTS, "settings"))
    parser.add_argument("--project-dir", type=Path, default=None)
    args = parser.parse_args(argv)
    if args.event == "settings":
        print(json.dumps(settings_block(), indent=2))
        return 0
    real_stdout = sys.stdout
    raw = sys.stdin.buffer.read()
    result, outcomes = runner.dispatch(args.event, raw, _root(args.project_dir))
    if os.environ.get("HOOK_DISPATCH_TRACE"):
        for outcome in outcomes:
            status = outcome.error or ("json" if outcome.output else "quiet")
            print(
                f"[dispatch] {outcome.name:<24} {outcome.elapsed_ms:7.1f} ms  {status}",
                file=sys.stderr,
            )
    real_stdout.write(json.dumps(result))
    real_stdout.write("\n")
    real_stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
