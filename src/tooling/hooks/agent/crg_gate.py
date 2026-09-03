#!/usr/bin/env python3
"""Session-scoped enforcement for the mandatory code-review-graph gate."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import time
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

STATE_TTL_SECONDS = 2 * 24 * 60 * 60
GRAPH_TOOL_PREFIXES = (
    "mcp__code_review_graph__",
    "mcp__code-review-graph__",
)
# The checkout served: CRG_GATE_REPO_ROOT (tests), else PROJECT_DIR (the exec
# launcher at the old hook path), else the layout <root>/tooling/hooks/agent/.
REPO_ROOT = Path(
    os.environ.get("CRG_GATE_REPO_ROOT")
    or os.environ.get("PROJECT_DIR")
    or Path(__file__).resolve().parents[3]
).resolve()
# A fail-open in a sibling worktree is recorded here rather than denied unless
# CRG_GATE_ENFORCE_WORKTREES is set. Enforcing it blocks every lane that has
# never claimed, which is a fleet decision, not a hook default.
EXPOSURE_CAP_BYTES = 4 * 1024 * 1024


def _checkout_of(path: Path) -> tuple[Path, Path] | None:
    """`(worktree root, git common dir)` for `path`, or None outside a checkout.

    Filesystem-only by design: this runs on every Edit, and a `git rev-parse`
    per target would put a subprocess in the hook's latency path. Walks
    lexically, so a path that does not exist yet (a Write creating a file)
    resolves the same as one that does.
    """
    for candidate in (path, *path.parents):
        entry = candidate / ".git"
        if entry.is_dir():
            return candidate, entry.resolve()
        if not entry.is_file():
            continue
        # A linked worktree: `.git` is a file pointing at
        # `<common>/worktrees/<name>`, which names the common dir in `commondir`.
        try:
            text = entry.read_text(encoding="utf-8").strip()
        except OSError:
            return None
        if not text.startswith("gitdir:"):
            return None
        gitdir = Path(text[len("gitdir:") :].strip())
        if not gitdir.is_absolute():
            gitdir = candidate / gitdir
        gitdir = gitdir.resolve()
        marker = gitdir / "commondir"
        if not marker.is_file():
            return candidate, gitdir
        try:
            raw = marker.read_text(encoding="utf-8").strip()
        except OSError:
            return None
        common = Path(raw)
        return candidate, (
            common if common.is_absolute() else (gitdir / common)
        ).resolve()
    return None


_REPO_CHECKOUT = _checkout_of(REPO_ROOT)
REPO_COMMON_DIR = _REPO_CHECKOUT[1] if _REPO_CHECKOUT else None


def _enforce_worktrees() -> bool:
    return os.environ.get("CRG_GATE_ENFORCE_WORKTREES", "") not in ("", "0")


def _record_exposure(owner: str, target: str, tool: str, checkout: str) -> None:
    """Append an unclaimed sibling-worktree write to a durable log.

    `create_claim` prunes expired claims on every write, so the claim store
    cannot answer "was this path claimed when it was written" for any past
    window. This log is the only record of the fail-open that survives that
    prune. It lives beside the claim store in the git common directory, so
    every worktree appends to one file and no checkout is dirtied.
    """
    if REPO_COMMON_DIR is None:
        return
    try:
        directory = REPO_COMMON_DIR / "governance"
        directory.mkdir(parents=True, exist_ok=True)
        path = directory / "claim-gate-exposure.jsonl"
        if path.is_file() and path.stat().st_size > EXPOSURE_CAP_BYTES:
            return
        line = json.dumps(
            {
                "at": time.time(),
                "owner": owner,
                "tool": tool,
                "checkout": checkout,
                "path": target,
            },
            sort_keys=True,
        )
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
        with os.fdopen(fd, "a", encoding="utf-8") as handle:
            handle.write(f"{line}\n")
    except OSError:
        return


def _read_payload() -> dict[str, Any]:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        return {}
    return payload if isinstance(payload, dict) else {}


def _state_dir() -> Path:
    path = Path(
        os.environ.get("CRG_GATE_STATE_DIR", "/tmp/claude-crg-gate")  # nosec B108
    )
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.chmod(0o700)
    return path


def _state_key(payload: dict[str, Any]) -> str | None:
    session_id = payload.get("session_id") or payload.get("sessionId")
    if not isinstance(session_id, str) or not session_id:
        return None
    return hashlib.sha256(session_id.encode()).hexdigest()


def _state_path(state_dir: Path, key: str, suffix: str) -> Path:
    return state_dir / f"{key}.{suffix}"


def _write_state(path: Path) -> None:
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(f"{time.time():.6f}\n")


def _prune_stale_state(state_dir: Path) -> None:
    cutoff = time.time() - STATE_TTL_SECONDS
    for path in state_dir.glob("*.*"):
        if path.suffix not in {".pending", ".graph-used"}:
            continue
        try:
            if path.is_file() and path.stat().st_mtime < cutoff:
                path.unlink()
        except OSError:
            continue


def _deny(payload: dict[str, Any], reason: str) -> None:
    if "toolName" in payload or payload.get("hookEventName") == "pre_tool_use":
        output: dict[str, Any] = {"decision": "deny", "reason": reason}
    else:
        output = {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        }
    print(json.dumps(output))


def _tool_input(payload: dict[str, Any]) -> dict[str, Any]:
    value = payload.get("tool_input") or payload.get("toolInput")
    return value if isinstance(value, dict) else {}


def _target_paths(payload: dict[str, Any]) -> list[str]:
    tool_input = _tool_input(payload)
    paths: list[str] = []
    for key in ("file_path", "filePath", "path", "notebook_path", "target_file"):
        value = tool_input.get(key)
        if isinstance(value, str) and value:
            paths.append(value)
    patch_candidates = (
        tool_input.get("patch"),
        tool_input.get("input"),
        tool_input.get("command"),
        payload.get("patch"),
        payload.get("input"),
        payload.get("command"),
    )
    for patch in patch_candidates:
        if isinstance(patch, str):
            paths.extend(
                match.group(1).strip()
                for match in re.finditer(
                    r"^\*\*\* (?:Add|Update|Delete) File: (.+)$",
                    patch,
                    flags=re.MULTILINE,
                )
            )
    return paths


def _classify_targets(
    payload: dict[str, Any],
) -> tuple[list[str], list[tuple[str, str]]]:
    """Split write targets into `(this checkout, sibling worktrees)`.

    Both halves are checkout-relative. Claims are keyed on the path within a
    checkout and `claim_store_path` resolves through the git common directory,
    so one claim covers the same path in every worktree of this repository --
    which is why a sibling worktree is gate-relevant and `/tmp` is not.
    """
    local: list[str] = []
    sibling: list[tuple[str, str]] = []
    for raw in _target_paths(payload):
        target = Path(raw)
        resolved = (
            target.resolve() if target.is_absolute() else (REPO_ROOT / target).resolve()
        )
        try:
            local.append(resolved.relative_to(REPO_ROOT).as_posix())
            continue
        except ValueError:
            pass
        checkout = _checkout_of(resolved)
        if checkout is None or REPO_COMMON_DIR is None:
            continue  # outside any checkout: the scratchpad, /tmp
        root, common = checkout
        if common != REPO_COMMON_DIR:
            continue  # a different repository entirely
        sibling.append((str(root), resolved.relative_to(root).as_posix()))
    return local, sibling


def _repo_relative_targets(payload: dict[str, Any]) -> list[str]:
    return _classify_targets(payload)[0]


def _claim_allows(owner: str, target: str) -> tuple[bool, str]:
    if not owner:
        return False, "hook has no GOVERNANCE_OWNER identity"
    try:
        sys.path.insert(0, str(REPO_ROOT))
        from conductor.candidate_review.ownership import (
            load_claims,
            paths_overlap,
            touch_claim,
        )

        claims, digest = load_claims(REPO_ROOT)
    except (ImportError, OSError, RuntimeError, ValueError) as exc:
        return False, f"live claim store is unavailable: {exc}"
    now = datetime.now(UTC)
    holders: list[str] = []
    lapsed: list[str] = []
    for claim in claims:
        if not any(paths_overlap(target, claimed) for claimed in claim.paths):
            continue
        mine = claim.owner.casefold() == owner.casefold()
        if not claim.active(now):
            if mine:
                lapsed.append(f"{claim.claim_id} ({claim.lapse_reason(now)})")
            continue
        if mine:
            try:
                touch_claim(REPO_ROOT, claim.claim_id, now=now)
            except OSError:
                # The stamp is a convenience, not the authority. If it cannot be
                # written the claim simply lapses on its existing timer, which is
                # the safe direction; refusing a legitimate write is not.
                pass
            return True, digest
        holders.append(
            f"{claim.owner} until {claim.deadline:%Y-%m-%dT%H:%M}Z ({claim.claim_id})"
        )
    if holders:
        return False, (
            f"path {target!r} is held by {'; '.join(holders)}; "
            f"owner={owner!r} has no live claim on it — coordinate via A2A or "
            "wait for expiry"
        )
    if lapsed:
        return False, (
            f"your claim on {target!r} is no longer live: {'; '.join(lapsed)} — "
            "re-claim the path before writing"
        )
    return False, f"no live exact claim for owner={owner!r} path={target!r}"


def _default_owner() -> str:
    explicit = os.environ.get("GOVERNANCE_OWNER", "").strip()
    if explicit:
        return explicit
    if os.environ.get("QWEN_PROJECT_DIR"):
        return "qwen"
    if os.environ.get("GROK_PROJECT_DIR"):
        return "grok"
    if os.environ.get("CLAUDE_PROJECT_DIR"):
        return os.environ.get("A2A_AGENT_NAME", "").strip() or "claude"
    if os.environ.get("CODEX_HOME"):
        return "codex"
    return "codex"


def start(payload: dict[str, Any]) -> int:
    state_dir = _state_dir()
    _prune_stale_state(state_dir)
    key = _state_key(payload)
    if key is not None:
        _write_state(_state_path(state_dir, key, "pending"))
    return 0


def mark(payload: dict[str, Any]) -> int:
    tool_name = payload.get("tool_name") or payload.get("toolName")
    key = _state_key(payload)
    if (
        key is not None
        and isinstance(tool_name, str)
        and tool_name.startswith(GRAPH_TOOL_PREFIXES)
    ):
        state_dir = _state_dir()
        _write_state(_state_path(state_dir, key, "graph-used"))
        _state_path(state_dir, key, "pending").unlink(missing_ok=True)
    return 0


def verify(payload: dict[str, Any], *, owner: str) -> int:
    key = _state_key(payload)
    if key is None or not _state_path(_state_dir(), key, "graph-used").is_file():
        _deny(
            payload,
            "BLOCKED: call a code-review-graph MCP tool before editing or writing in this session.",
        )
        return 0
    if not _target_paths(payload):
        _deny(
            payload, "BLOCKED: edit target is missing; live claim cannot be verified."
        )
        return 0
    # Paths outside any checkout (session scratchpad, /tmp) have nothing to claim.
    local, sibling = _classify_targets(payload)
    for target in local:
        allowed, detail = _claim_allows(owner, target)
        if not allowed:
            _deny(
                payload, f"BLOCKED: {detail}. Create a narrow governance claim first."
            )
            return 0
    tool = payload.get("tool_name") if isinstance(payload.get("tool_name"), str) else ""
    for checkout, target in sibling:
        allowed, detail = _claim_allows(owner, target)
        if allowed:
            continue
        _record_exposure(owner, target, tool or "", checkout)
        if _enforce_worktrees():
            _deny(
                payload,
                f"BLOCKED: {detail} (in worktree {checkout}). "
                "Create a narrow governance claim first.",
            )
            return 0
    return 0


def _bash_command(payload: dict[str, Any]) -> str:
    value = _tool_input(payload).get("command")
    return value if isinstance(value, str) else ""


def _bash_checkout(payload: dict[str, Any]) -> tuple[Path, bool]:
    """`(checkout to resolve write targets against, is a sibling worktree)`."""
    cwd = payload.get("cwd")
    if isinstance(cwd, str) and cwd and REPO_COMMON_DIR is not None:
        checkout = _checkout_of(Path(cwd).resolve())
        if checkout and checkout[1] == REPO_COMMON_DIR and checkout[0] != REPO_ROOT:
            return checkout[0], True
    return REPO_ROOT, False


def verify_bash(payload: dict[str, Any], *, owner: str) -> int:
    """Apply the graph+claim gate to a Bash command that writes repo files.

    `verify` covers Edit/Write/NotebookEdit only, so `sed -i` or a heredoc
    redirect into a tracked file performed the same mutation with none of the
    enforcement. Read-only commands -- the overwhelming majority -- resolve to
    zero write targets and are allowed without any state requirement.
    """
    command = _bash_command(payload)
    if not command:
        return 0
    # Fail OPEN, unlike `verify`. This hook sees every Bash call in the fleet;
    # a parser bug that denied them all would be an availability incident far
    # worse than the loophole it closes. The Edit/Write path stays fail-closed.
    try:
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        from bash_write_targets import OPAQUE_WRITE, repo_write_targets

        # Resolve against the checkout the command actually runs in. Under the
        # hardcoded root an absolute path inside a sibling worktree fell
        # outside it and was dropped, so the write was never gated. A relative
        # one produced the same repo-relative string either way, so its claim
        # decision was accidentally right while attributed to the wrong tree.
        base, in_sibling = _bash_checkout(payload)
        targets = repo_write_targets(command, base)
    except Exception:  # noqa: BLE001 - see fail-open rationale above
        return 0
    if not targets:
        return 0
    if OPAQUE_WRITE in targets:
        _deny(
            payload,
            "BLOCKED: this Bash command writes repo files through an interpreter "
            "whose target cannot be resolved, so the claim gate cannot check it. "
            "Use Edit/Write, or name the path as a literal.",
        )
        return 0
    if not _state_path(_state_dir(), _state_key(payload) or "", "graph-used").is_file():
        _deny(
            payload,
            "BLOCKED: call a code-review-graph MCP tool before writing repo files "
            f"in this session (this command writes {', '.join(targets)}).",
        )
        return 0
    for target in targets:
        allowed, detail = _claim_allows(owner, target)
        if allowed:
            continue
        if in_sibling:
            _record_exposure(owner, target, "Bash", str(base))
            if not _enforce_worktrees():
                continue
        _deny(payload, f"BLOCKED: {detail}. Create a narrow governance claim first.")
        return 0
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("start", "mark", "verify", "verify-bash"))
    parser.add_argument("--owner", default=_default_owner())
    args = parser.parse_args()
    payload = _read_payload()
    if args.action == "start":
        return start(payload)
    if args.action == "mark":
        return mark(payload)
    if args.action == "verify-bash":
        return verify_bash(payload, owner=args.owner)
    return verify(payload, owner=args.owner)


if __name__ == "__main__":
    raise SystemExit(main())
