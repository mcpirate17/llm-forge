#!/usr/bin/env python3
"""Stub standing in for ``tooling.hooks.dispatch`` in forge's Rust integration tests.

Installed as the fake project's ``.venv/bin/python``: ``forge hook <event>`` execs
this file with args ``["-m", "tooling.hooks.dispatch", <event>]`` exactly like it
would exec a real interpreter for ``python -m tooling.hooks.dispatch <event>`` --
this script never needs to *be* Python's ``-m`` machinery, only what
``interpreter::resolve_python`` finds at ``.venv/bin/python`` and what
``dispatch::run_hook`` launches.

Behavior is keyed on the event name so the Rust tests can drive every path forge
must forward byte-for-byte: stdin passthrough, stdout, stderr, and exit codes 0
(allow), 2 (deny) and a nonzero non-2 code (error).
"""

from __future__ import annotations

import json
import sys


def main() -> int:
    # sys.argv[1:] == ["-m", "tooling.hooks.dispatch", <event>]
    event = sys.argv[-1]
    stdin_payload = sys.stdin.read()

    if event == "PreToolUse":
        import os

        sys.stdout.write(
            json.dumps(
                {
                    "echoed": json.loads(stdin_payload),
                    "forge_native_hooks": os.environ.get("FORGE_NATIVE_HOOKS"),
                    "forge_native_answers": os.environ.get("FORGE_NATIVE_ANSWERS"),
                }
            )
        )
        return 0
    if event in ("SessionStart", "SessionEnd"):
        import os

        sys.stdout.write(
            json.dumps(
                {
                    "event": event,
                    "echoed": json.loads(stdin_payload or "{}"),
                    "forge_native_hooks": os.environ.get("FORGE_NATIVE_HOOKS"),
                    "forge_native_answers": os.environ.get("FORGE_NATIVE_ANSWERS"),
                }
            )
        )
        return 0
    if event == "PostToolUse":
        sys.stderr.write("stub dispatcher: soft warning on stderr\n")
        sys.stdout.write(json.dumps({"ok": True}))
        return 0
    if event == "Deny":
        sys.stdout.write(json.dumps({"decision": "block", "reason": "stub deny"}))
        return 2
    if event == "Explode":
        sys.stderr.write("stub dispatcher: fatal\n")
        return 7
    raise SystemExit(f"stub_dispatch.py: unhandled test event {event!r}")


if __name__ == "__main__":
    raise SystemExit(main())
