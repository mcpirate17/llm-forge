"""The governance commit lock's record must not outlive its holder.

`_held_governance_lock` writes `{pid, token, acquired}` into the lock file so a
child process can prove it inherited its parent's lease -- `_inherited_lock_token_valid`
reads exactly those fields. Release dropped the flock and left the record behind, so
the file went on naming a pid and a token with no holder anywhere. A separate module
because `test_candidate_review.py` is already over the size cap on a dated exception.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

from conductor.candidate_review import engine as review_engine


def _init_repo(path: Path) -> Path:
    path.mkdir(parents=True)
    subprocess.run(["git", "init", "-q"], cwd=path, check=True)
    return path


def test_released_governance_lock_leaves_no_lease_claim(tmp_path: Path) -> None:
    """A released lock must not still name a pid and a token.

    `_inherited_lock_token_valid` reads that record to decide whether a child may
    reuse its parent's lease. A record that outlives its holder is a claim with
    nothing behind it: the live file said "held by pid 3457064" long after it exited.
    """
    repo = _init_repo(tmp_path / "repo")
    lock_path = review_engine._governance_lock_path(repo)  # noqa: SLF001
    token = "c" * 64
    with review_engine._held_governance_lock(  # noqa: SLF001
        repo, exclusive=True, timeout_seconds=1.0, lease_token=token
    ):
        held = json.loads(lock_path.read_text(encoding="utf-8"))
        assert held["pid"] == os.getpid()
        assert held["token"] == token
    assert lock_path.read_text(encoding="utf-8") == ""


def test_release_does_not_erase_another_holders_record(tmp_path: Path) -> None:
    """Only our own record is erased -- a shared holder must not blank a peer's.

    Truncating unconditionally would let one reader's exit delete the record another
    reader still relies on, turning a hygiene fix into the lease-loss fault it was
    written to prevent.
    """
    repo = _init_repo(tmp_path / "repo")
    lock_path = review_engine._governance_lock_path(repo)  # noqa: SLF001
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    peer = json.dumps({"pid": os.getpid() + 1, "token": "d" * 64}, sort_keys=True)
    with review_engine._held_governance_lock(  # noqa: SLF001
        repo, exclusive=True, timeout_seconds=1.0, lease_token="e" * 64
    ):
        lock_path.write_text(peer, encoding="utf-8")
    assert lock_path.read_text(encoding="utf-8") == peer


def test_an_unreadable_record_does_not_break_release(tmp_path: Path) -> None:
    """Release must survive a record it cannot parse.

    The lock is held across a whole review; raising out of the `finally` that drops
    the flock would leave the mutex held for the life of the process and wedge every
    later commit. Garbage in the file is a diagnostic loss, never a stuck lock.
    """
    repo = _init_repo(tmp_path / "repo")
    lock_path = review_engine._governance_lock_path(repo)  # noqa: SLF001
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with review_engine._held_governance_lock(  # noqa: SLF001
        repo, exclusive=True, timeout_seconds=1.0, lease_token="f" * 64
    ):
        lock_path.write_text("not json at all\n", encoding="utf-8")
    assert lock_path.read_text(encoding="utf-8") == "not json at all\n"
    with review_engine._held_governance_lock(  # noqa: SLF001
        repo, exclusive=True, timeout_seconds=1.0, lease_token="f" * 64
    ):
        pass
