"""One identity for claiming a path and for writing it.

The claim store is only a control if the name a lane claims under is the name its
write gate checks. It was not. ``.codex/hooks.json`` pinned ``GOVERNANCE_OWNER=codex``
while codex sessions created claims as ``codex-audit-repository-scan-rust-20260903``,
so codex's gate could not recognise codex's own claims; every Claude session resolved
to the bare literal ``claude``, so any one of them could write over another's claim.
Both ends now call :func:`resolve_owner`.

The name is the **lane**, not the vendor. A lane is one worktree carrying one piece of
work, which is exactly the unit claims are meant to separate -- four codex worktrees
are four lanes, and calling them all ``codex`` is the same as not claiming. A vendor
literal is therefore not an identity here: it is ignored wherever it appears, including
in an exported ``GOVERNANCE_OWNER``, so a lane inherits a real name without every
launcher having to be edited first.

Derivation is filesystem-only. This runs on every Edit, Write and Bash in the fleet;
a ``git rev-parse`` per call would put a subprocess in the hook's latency path.
"""

from __future__ import annotations

import os
import re
from pathlib import Path
from typing import Mapping

# Vendor names are what the fleet used before lanes had identities. They stay
# resolvable (a lane with no derivable name still gets *something*, and the gate
# still honours claims written under them) but they are never a lane identity.
VENDOR_MARKERS: tuple[tuple[str, str], ...] = (
    ("QWEN_PROJECT_DIR", "qwen"),
    ("GROK_PROJECT_DIR", "grok"),
    ("CLAUDE_PROJECT_DIR", "claude"),
    ("CODEX_HOME", "codex"),
)
VENDORS: frozenset[str] = frozenset(vendor for _marker, vendor in VENDOR_MARKERS)
_OWNER_MAX = 64
_UNSAFE = re.compile(r"[^a-z0-9._-]+")


class OwnerIdentityError(RuntimeError):
    """The running lane cannot be named, so nothing may be claimed or written."""


def is_vendor(owner: str) -> bool:
    return owner.strip().casefold() in VENDORS


def vendor_of(env: Mapping[str, str] | None = None) -> str:
    """The vendor whose launcher this process is under, or ``""``."""
    env = os.environ if env is None else env
    for marker, vendor in VENDOR_MARKERS:
        if (env.get(marker) or "").strip():
            return vendor
    return ""


def vendor_for(owner: str, env: Mapping[str, str] | None = None) -> str:
    """The vendor a lane belongs to: its launcher's, else its own name prefix.

    The launcher marker is absent whenever the gate is reached from a plain shell,
    so the lane name has to be able to answer on its own. Lanes are named
    ``<vendor>-<work>-<date>`` by the worktree convention, which makes the prefix
    a reliable second source and keeps the legacy-claim fallback from silently
    going dark in exactly the sessions that still need it.
    """
    marker = vendor_of(env)
    if marker:
        return marker
    head = normalize(owner).split("-", 1)[0]
    return head if head in VENDORS else ""


def normalize(owner: str) -> str:
    """Fold an arbitrary lane string into the owner charset, or ``""``."""
    folded = _UNSAFE.sub("-", owner.strip().casefold()).strip("-.")
    return folded[:_OWNER_MAX]


def lane_of(repo_root: Path) -> str:
    """The lane name of a checkout: its worktree name, else its branch.

    A linked worktree is named for the work it carries, and keeps that name across
    branch switches, so it is the better key. The main checkout is named ``LLM``,
    which is shared by every session in it, so there the branch is the lane.
    """
    marker = repo_root / ".git"
    if marker.is_file():
        return normalize(repo_root.name)
    if not marker.is_dir():
        return ""
    try:
        head = (marker / "HEAD").read_text(encoding="utf-8").strip()
    except OSError:
        return ""
    if not head.startswith("ref: refs/heads/"):
        return ""  # detached: no branch, so no lane
    return normalize(head[len("ref: refs/heads/") :])


def resolve_owner(
    repo_root: Path | None = None, env: Mapping[str, str] | None = None
) -> str:
    """The identity this lane claims and writes under.

    An exported ``GOVERNANCE_OWNER`` wins -- that is a lane declaring its own name --
    unless it is a bare vendor literal, which is what the old launchers pinned and
    what this exists to stop. Otherwise the lane is derived from the checkout, and
    only if that fails does the vendor stand in.
    """
    env = os.environ if env is None else env
    declared = normalize(env.get("GOVERNANCE_OWNER") or "")
    if declared and not is_vendor(declared):
        return declared
    if repo_root is not None:
        lane = lane_of(repo_root)
        if lane and not is_vendor(lane):
            return lane
    vendor = vendor_of(env) or declared
    if vendor:
        return vendor
    raise OwnerIdentityError(
        "no governance identity: this process is under no known agent launcher and "
        "its checkout has no lane name — export GOVERNANCE_OWNER=<lane> "
        "(a worktree or branch name, not a vendor)"
    )


def require_lane_owner(owner: str) -> str:
    """Validate an owner about to be written into a claim."""
    folded = normalize(owner)
    if not folded:
        raise OwnerIdentityError(f"owner {owner!r} is empty after normalization")
    if is_vendor(folded):
        raise OwnerIdentityError(
            f"owner {folded!r} names a vendor, not a lane: every session of that "
            "vendor would satisfy this claim. Claim under the worktree or branch "
            "name, or export GOVERNANCE_OWNER=<lane>"
        )
    return folded
