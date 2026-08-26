#!/usr/bin/env python3
"""Active state manager for AVO Tier-0 token-conscious state compression.

Generates and maintains ``conductor/active_state.json``, which provides a compact
(<500 token) summary of active coordination, standing mandates, and claims so
agents do not need to parse large markdown files on every turn.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Final

ROOT: Final[Path] = Path(__file__).resolve().parents[1]
ACTIVE_STATE_PATH: Final[Path] = ROOT / "conductor" / "active_state.json"
CURRENT_WORK_PATH: Final[Path] = ROOT / ".current_work.md"
CLAIMS_PATH: Final[Path] = ROOT / ".agents" / "claims" / "claims.json"

HEADING_RE: Final = re.compile(r"^##\s+(.+)$")


@dataclass
class ActiveState:
    """Compact machine-readable state representation for agent sessions."""

    schema_version: int = 1
    last_updated: str = field(
        default_factory=lambda: dt.datetime.now(dt.timezone.utc).isoformat()
    )
    standing_mandates: list[str] = field(
        default_factory=lambda: [
            "NOVEL_MECHANISMS_ONLY: Never replace novel lane with softmax/QKV twins. Gate drops are defects to fix.",
            "GRAPH_GATE: Call code-review-graph MCP before any Edit/Write.",
            "EAGER_REQUIRED: Paired comparisons and loss-sensitive probes use --compile-mode default.",
            "CLAIM_REQUIRED: Create narrow claim before editing (make governance-claim).",
            "AVO_USER_GATED: Autonomous variation (AVO) loops are user-invoked only. When a task is a continuous-improvement goal (iterative metric optimization, variation/evolution loops), prompt Tim first — 'This is a continuous-improvement goal — invoke AVO?' — and wait for his answer before starting any loop.",
        ]
    )
    active_headings: list[str] = field(default_factory=list)
    active_claims: list[dict[str, Any]] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def parse_top_headings(limit: int = 4) -> list[str]:
    """Parse top active headings from .current_work.md."""
    if not CURRENT_WORK_PATH.exists():
        return []
    headings: list[str] = []
    try:
        with CURRENT_WORK_PATH.open("r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                m = HEADING_RE.match(line)
                if m:
                    heading_text = m.group(1).strip()
                    if heading_text.lower() != "active coordination":
                        headings.append(heading_text)
                        if len(headings) >= limit:
                            break
    except OSError:
        pass
    return headings


def parse_active_claims() -> list[dict[str, Any]]:
    """Read unexpired claims from the governance ownership store."""
    try:
        from conductor.candidate_review.ownership import load_claims

        claims, _ = load_claims(ROOT)
        now = dt.datetime.now(dt.timezone.utc)
        active: list[dict[str, Any]] = []
        for c in claims:
            if c.active(now):
                active.append(
                    {
                        "claim_id": c.claim_id,
                        "owner": c.owner,
                        "paths": list(c.paths),
                        "justification": c.justification,
                        "expires_at": c.expires_at,
                    }
                )
        return active
    except Exception:
        return []


def generate_active_state() -> ActiveState:
    """Construct an updated ActiveState object from live repo sources."""
    headings = parse_top_headings(limit=4)
    claims = parse_active_claims()
    return ActiveState(
        active_headings=headings,
        active_claims=claims,
    )


def save_active_state(path: Path = ACTIVE_STATE_PATH) -> ActiveState:
    """Generate and write active_state.json."""
    state = generate_active_state()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(state.to_dict(), indent=2) + "\n", encoding="utf-8")
    return state


def cmd_update() -> int:
    save_active_state()
    return 0


def cmd_dump() -> int:
    state = generate_active_state()
    print(json.dumps(state.to_dict(), indent=2))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Manage AVO Tier-0 active state.")
    parser.add_argument(
        "action",
        nargs="?",
        choices=["update", "dump"],
        default="update",
        help="Action to perform (default: update)",
    )
    args = parser.parse_args(argv)
    if args.action == "dump":
        return cmd_dump()
    return cmd_update()


if __name__ == "__main__":
    sys.exit(main())
