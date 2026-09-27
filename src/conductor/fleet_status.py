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

from conductor.project_paths import host_root, worktree_patterns

ROOT: Final[Path] = host_root()
ACTIVE_STATE: Final[Path] = ROOT / "conductor" / "active_state.json"
HELM_SEAT: Final[str] = "fable-helm"
_HEADING_SEAT = re.compile(r",\s*([\w.-]+)\s*$")
# Host-configured via `[tool.conductor].worktree_patterns` (see project_paths);
# defaults to this monorepo's own layout so nothing regresses on this host.
_WORKTREE = re.compile("(" + "|".join(worktree_patterns(ROOT)) + ")")
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
            "--compact",
            "--json",
            "--max-messages",
            "8",
            "--max-chars",
            "10000",
        ]
    )
    latest: dict[str, dict[str, str]] = {}
    try:
        payload = json.loads(out)
        for message in payload["messages"]:
            sender, at, summary = (message["from"], message["at"], message["summary"])
            if not all(isinstance(value, str) for value in (sender, at, summary)):
                raise ValueError(
                    "message sender, timestamp and summary must be strings"
                )
            # Compact views may order by thread/actionability. Compare timestamps
            # explicitly instead of depending on presentation order.
            if sender not in latest or at > latest[sender]["at"]:
                latest[sender] = {"at": at, "said": summary[:160]}
    except (ValueError, KeyError, TypeError) as exc:
        raise FleetStatusError(f"invalid compact A2A inbox: {exc}") from exc
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

    from conductor._native import fleet_status_build_report_native

    return json.loads(
        fleet_status_build_report_native(
            json.dumps(peers, ensure_ascii=False),
            json.dumps(heard, ensure_ascii=False),
            json.dumps(
                {"active_claims": claims, "active_headings": headings},
                ensure_ascii=False,
            ),
            json.dumps(read_worktree_procs(), ensure_ascii=False),
            datetime.now(UTC).isoformat(timespec="seconds"),
            str(ROOT),
        )
    )


def render(report: dict[str, Any]) -> str:
    from conductor._native import fleet_status_render_native

    return fleet_status_render_native(json.dumps(report, ensure_ascii=False))


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
