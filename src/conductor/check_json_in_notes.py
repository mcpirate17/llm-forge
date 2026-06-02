#!/usr/bin/env python3
"""Pre-commit hook: reject the retired research/notes/ tree.

Durable research notes now live in the Obsidian vault. Persistent JSON inputs
belong under research/data/, and disposable tool outputs belong under
research/reports/ or tasks/audit/.
"""

from __future__ import annotations

import re
import sys

FORBIDDEN_RE = re.compile(r"^research/notes(?:/|$)")


def main() -> int:
    bad = [f for f in sys.argv[1:] if FORBIDDEN_RE.match(f)]
    if not bad:
        return 0
    for f in bad:
        sys.stderr.write(
            f"research/notes/ is retired: {f} — use Obsidian for notes, "
            "research/data/ for persistent inputs, or research/reports/ for outputs\n"
        )
    return 1


if __name__ == "__main__":
    sys.exit(main())
