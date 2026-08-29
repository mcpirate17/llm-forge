"""Deterministic Grok inspect response for isolated governance campaigns."""

from __future__ import annotations

import json
from pathlib import Path


def main() -> None:
    root = Path.cwd().resolve()
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
