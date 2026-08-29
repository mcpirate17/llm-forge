"""Deterministic Grok inspect response for isolated governance campaigns."""

from __future__ import annotations

import json
import sys
from pathlib import Path


def main(argv: list[str] | None = None) -> None:
    """Print trusted-project hook discovery for ``argv[0]`` (default: cwd)."""
    args = sys.argv[1:] if argv is None else argv
    if len(args) > 1:
        raise SystemExit(f"usage: grok_inspect_stub [project-root]; got {args!r}")
    root = (Path(args[0]) if args else Path.cwd()).resolve()
    print(
        json.dumps(
            {
                "projectRoot": str(root),
                "projectTrusted": True,
                "hooks": [
                    {
                        "event": "pre_tool_use",
                        "source": {"path": str(root / ".grok" / "hooks")},
                        "matcher": "Read|read|read_file",
                    },
                    {
                        "event": "pre_tool_use",
                        "source": {"path": str(root / ".grok" / "hooks")},
                        "matcher": "Bash|run_shell_command|shell",
                    },
                ],
            }
        )
    )


if __name__ == "__main__":
    main()
