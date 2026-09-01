#!/usr/bin/env python3
"""One-screen fleet view: what every seat is doing right now.

Joins four read-only sources — A2A peer liveness, the helm inbox (last words
per seat), active_state claims/headings, and live processes grouped by
worktree. Codex terminals have no per-session title view, so this is the
"what is each codex doing" answer in one command:

    .venv/bin/python -m conductor.fleet_status [--json]
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Final

ROOT: Final[Path] = Path(__file__).resolve().parents[1]
ACTIVE_STATE: Final[Path] = ROOT / "conductor" / "active_state.json"
HELM_SEAT: Final[str] = "fable-helm"
INBOX_LIMIT: Final[int] = 40
_INBOX_HEADER = re.compile(r"^\[(UNREAD|READ)\]\s+(\S+)\s+from=(\S+)\s+at=(\S+)")
_HEADING_SEAT = re.compile(r",\s*([\w.-]+)\s*$")
_WORKTREE = re.compile(r"(/tmp/llm-[\w.-]+|/home/\w+/Projects/LLM[\w.-]*)")
# Display classification only (which trees get per-process detail); no temp
# files are created here.
_DISPOSABLE_TREE = re.compile(r"^/tmp/")


class FleetStatusError(RuntimeError):
    """A required source could not be read."""


def _run(argv: list[str]) -> str:
    proc = subprocess.run(
        argv, capture_output=True, text=True, timeout=30, cwd=ROOT, check=False
    )
    if proc.returncode != 0:
        raise FleetStatusError(
            f"{' '.join(argv[:4])}… exited {proc.returncode}: {proc.stderr.strip()[:200]}"
        )
    return proc.stdout


def read_peers() -> dict[str, dict[str, Any]]:
    out = _run([sys.executable, "-m", "conductor.agent_a2a", "peers"])
    return {p["name"]: p for p in json.loads(out)}


def read_last_heard(as_name: str = HELM_SEAT) -> dict[str, dict[str, str]]:
    """Latest inbound message per sender, from the helm inbox (peek only)."""
    out = _run(
        [
            sys.executable,
            "-m",
            "conductor.agent_a2a",
            "inbox",
            "--as-name",
            as_name,
            "--limit",
            str(INBOX_LIMIT),
        ]
    )
    latest: dict[str, dict[str, str]] = {}
    lines = out.splitlines()
    for i, line in enumerate(lines):
        m = _INBOX_HEADER.match(line)
        if not m:
            continue
        sender, at = m.group(3), m.group(4)
        body = lines[i + 1].strip() if i + 1 < len(lines) else ""
        # inbox is newest-first; keep only the first (latest) entry per sender
        latest.setdefault(sender, {"at": at, "said": body[:160]})
    return latest


def read_state() -> dict[str, Any]:
    # session_preamble.load_state refreshes the canonical cache first, so
    # claims created seconds ago are visible; a raw file read shows stale data.
    from conductor.session_preamble import load_state

    return load_state()


def read_worktree_procs() -> dict[str, list[str]]:
    out = _run(["ps", "ax", "-o", "pid=,etime=,args="])
    procs: dict[str, list[str]] = {}
    for line in out.splitlines():
        if "fleet_status" in line:
            continue
        m = _WORKTREE.search(line)
        if m:
            procs.setdefault(m.group(1), []).append(line.strip()[:140])
    return procs


def _heading_seat(heading: str) -> str:
    m = _HEADING_SEAT.search(heading)
    return m.group(1) if m else ""


def build_report() -> dict[str, Any]:
    peers = read_peers()
    heard = read_last_heard()
    state = read_state()
    claims = state.get("active_claims", [])
    headings = state.get("active_headings", [])

    seats: dict[str, dict[str, Any]] = {}
    names = set(peers) | set(heard) | {c.get("owner", "") for c in claims}
    names |= {_heading_seat(h) for h in headings}
    names.discard("")
    for name in sorted(names):
        peer = peers.get(name)
        seat_claims = [c for c in claims if c.get("owner") == name]
        expiries = sorted(
            c.get("expires_at", "") for c in seat_claims if c.get("expires_at")
        )
        seats[name] = {
            "a2a": (
                f"up:{peer['port']}"
                if peer and peer.get("status") == "up"
                else "down"
                if peer
                else "NO IDENTITY"
            ),
            "last_heard": heard.get(name),
            "headings": [h for h in headings if _heading_seat(h) == name],
            "claims": len(seat_claims),
            "claim_paths": sorted({p for c in seat_claims for p in c.get("paths", [])}),
            "soonest_expiry": expiries[0] if expiries else None,
        }
    return {
        "generated_at": datetime.now(UTC).isoformat(timespec="seconds"),
        "root": str(ROOT),
        "seats": seats,
        "worktree_processes": read_worktree_procs(),
    }


def render(report: dict[str, Any]) -> str:
    lines = [f"FLEET STATUS  {report['generated_at']}  root={report['root']}"]
    for name, s in report["seats"].items():
        expiry = (
            f"  soonest-expiry={s['soonest_expiry'][:16]}Z"
            if s["soonest_expiry"]
            else ""
        )
        lines.append(f"\n● {name}  [{s['a2a']}]  claims={s['claims']}{expiry}")
        for h in s["headings"]:
            lines.append(f"    heading: {h}")
        if s["last_heard"]:
            lines.append(
                f"    last heard {s['last_heard']['at'][:19]}: {s['last_heard']['said']}"
            )
        if s["claim_paths"]:
            shown = s["claim_paths"][:4]
            extra = len(s["claim_paths"]) - len(shown)
            lines.append(
                "    owns: " + ", ".join(shown) + (f" (+{extra} more)" if extra else "")
            )
    procs = report["worktree_processes"]
    if procs:
        lines.append("\nLIVE PROCESSES BY WORKTREE")
        for tree, ps in sorted(procs.items()):
            lines.append(f"  {tree}: {len(ps)}")
            if not _DISPOSABLE_TREE.match(tree):
                continue  # the shared checkout is long-lived daemons; count only
            for p in ps[:3]:
                lines.append(f"    {p}")
            if len(ps) > 3:
                lines.append(f"    … +{len(ps) - 3} more")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="One-screen fleet status")
    parser.add_argument(
        "--json", action="store_true", help="emit the raw report as JSON"
    )
    args = parser.parse_args(argv)
    report = build_report()
    print(json.dumps(report, indent=2) if args.json else render(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
