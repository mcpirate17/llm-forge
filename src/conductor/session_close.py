#!/usr/bin/env python3
"""Unified session retirement and handoff utility for LLM workspace agents.

Consolidates optional handoff status append, active state refresh,
and bounded claim release into a single, fail-closed operation.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Final

from conductor.active_state import save_active_state
from conductor.candidate_review.git_source import repository_root
from conductor.candidate_review.ownership import (
    OwnershipError,
    load_claims,
    release_claim,
)
from conductor.handoff import HandoffError, append_status

ROOT: Final[Path] = Path(__file__).resolve().parents[1]


class SessionCloseError(RuntimeError):
    """Session close could not be completed."""


@dataclass(frozen=True, slots=True)
class SessionCloseResult:
    owner: str
    claims_released: tuple[str, ...]
    handoff_entry: str | None
    active_headings_count: int
    active_claims_count: int
    memory_status: str

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def release_owner_claims(
    repo: Path,
    *,
    owner: str,
    claim_ids: tuple[str, ...] = (),
    all_owner_claims: bool = False,
) -> tuple[str, ...]:
    """Release specific claim IDs, or all claims for owner if explicitly authorized."""
    owner_clean = owner.strip()
    if not owner_clean:
        raise SessionCloseError("owner is required to release claims")
    if not claim_ids and not all_owner_claims:
        raise SessionCloseError(
            "specify --claim-id <id>... or explicit --all-owner-claims to release claims"
        )

    released: list[str] = []
    if claim_ids:
        for cid in claim_ids:
            release_claim(repo, claim_id=cid, owner=owner_clean)
            released.append(cid)
    elif all_owner_claims:
        claims, _ = load_claims(repo)
        for claim in claims:
            if claim.owner.casefold() == owner_clean.casefold():
                release_claim(repo, claim_id=claim.claim_id, owner=owner_clean)
                released.append(claim.claim_id)

    return tuple(released)


def close_session(
    repo: Path,
    *,
    owner: str,
    title: str | None = None,
    body: str | None = None,
    claim_ids: tuple[str, ...] = (),
    all_owner_claims: bool = False,
    sync_memory_index: bool = True,
) -> SessionCloseResult:
    """Atomically retire an agent session, updating state and releasing claims fail-closed."""
    owner_clean = owner.strip()
    if not owner_clean:
        raise SessionCloseError("owner is required")
    if (title is not None and body is None) or (title is None and body is not None):
        raise SessionCloseError("both --title and --body must be provided together")

    # Step 1: Append handoff status to .current_work.md first
    handoff_text: str | None = None
    if title is not None and body is not None:
        try:
            handoff_text = append_status(
                owner_clean, title, body, path=repo / ".current_work.md"
            )
        except HandoffError as exc:
            raise SessionCloseError(f"handoff append failed: {exc}") from exc

    # Step 2: Validate and refresh active_state.json
    try:
        active_state = save_active_state(repo / "conductor" / "active_state.json")
    except Exception as exc:
        raise SessionCloseError(f"active state refresh failed: {exc}") from exc

    # Step 3: Release claims only after state is successfully preserved
    released_claims: tuple[str, ...] = ()
    if claim_ids or all_owner_claims:
        released_claims = release_owner_claims(
            repo,
            owner=owner_clean,
            claim_ids=claim_ids,
            all_owner_claims=all_owner_claims,
        )
        # Refresh active state again to reflect released claims
        active_state = save_active_state(repo / "conductor" / "active_state.json")

    # Step 4: Memory indexing
    memory_status = "skipped"
    if sync_memory_index:
        try:
            from conductor.memory_index import build_index

            build_index(repo=repo)
            memory_status = "ok"
        except Exception as exc:
            memory_status = f"error: {exc}"
            print(
                f"session-close notice: memory index sync failed ({exc})",
                file=sys.stderr,
            )

    return SessionCloseResult(
        owner=owner_clean,
        claims_released=released_claims,
        handoff_entry=handoff_text.splitlines()[0] if handoff_text else None,
        active_headings_count=len(active_state.active_headings),
        active_claims_count=len(active_state.active_claims),
        memory_status=memory_status,
    )


def format_summary(result: SessionCloseResult) -> str:
    lines = [
        f"session-close SUCCESS | owner={result.owner}",
        f"- claims released: {len(result.claims_released)}"
        + (f" ({', '.join(result.claims_released)})" if result.claims_released else ""),
    ]
    if result.handoff_entry:
        lines.append(f"- handoff appended: {result.handoff_entry}")
    lines.append(
        f"- active state: {result.active_headings_count} headings, {result.active_claims_count} active claims"
    )
    lines.append(f"- memory index: {result.memory_status}")
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--owner", required=True, help="Agent owner identity")
    parser.add_argument(
        "--title", default=None, help="Optional handoff title (<=120 chars)"
    )
    parser.add_argument(
        "--body", default=None, help="Optional handoff body (<=12 lines)"
    )
    parser.add_argument(
        "--claim-id",
        dest="claim_ids",
        action="append",
        default=[],
        help="Specific claim ID to release (can be repeated)",
    )
    parser.add_argument(
        "--all-owner-claims",
        action="store_true",
        help="Explicitly release ALL claims owned by this owner",
    )
    parser.add_argument(
        "--no-memory-index", action="store_true", help="Skip memory index build"
    )
    parser.add_argument("--json", action="store_true", help="Output JSON receipt")
    parser.add_argument("--repo", default=str(ROOT), help="Repository root path")

    args = parser.parse_args(argv)
    try:
        repo = repository_root(Path(args.repo))
        result = close_session(
            repo,
            owner=args.owner,
            title=args.title,
            body=args.body,
            claim_ids=tuple(args.claim_ids),
            all_owner_claims=args.all_owner_claims,
            sync_memory_index=not args.no_memory_index,
        )
        if args.json:
            print(json.dumps(result.to_dict(), indent=2))
        else:
            sys.stdout.write(format_summary(result))
        return 0
    except (SessionCloseError, OwnershipError, OSError) as exc:
        print(f"session-close FAILED: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
