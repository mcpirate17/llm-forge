#!/usr/bin/env python3
"""Pre-commit hook: research/notes/ is for .md knowledge artifacts only.

Rejects new commits that add .json/.jsonl/.csv files at the top level of
research/notes/. Tool outputs go to research/reports/ (auto-pruned 14d).

Subdirs (mixer_fingerprint/, archive/) are exempt because they hold runtime
output and archival snapshots respectively.
"""

from __future__ import annotations

import re
import sys

FORBIDDEN_RE = re.compile(r"^research/notes/[^/]+\.(json|jsonl|csv)$")


def main() -> int:
    bad = [f for f in sys.argv[1:] if FORBIDDEN_RE.match(f)]
    if not bad:
        return 0
    for f in bad:
        sys.stderr.write(
            f"research/notes/ is .md-only: {f} — write tool outputs to research/reports/\n"
        )
    return 1


if __name__ == "__main__":
    sys.exit(main())
