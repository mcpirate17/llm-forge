"""Tests for the workspace hygiene report's fixed defects and its inject line.

Every behaviour pinned here was broken on main at the start of this slice:

* the reap decision judged containment against the ``origin/master`` literal,
  ignoring ``[tool.conductor].integration_branch`` and the remote's own HEAD
  symref, so on this repo (line ``main``) a finished worktree read as "run is
  not over";
* ``ROOT`` was the package parent (``src/``), so the full report died on the
  mutation registry and the claim query on a ``.venv/bin/python`` relative
  path, and idle-claim mtimes were read from ``src/<claim path>``;
* the claim query itself was a second interpreter.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from conductor import workspace_hygiene as wh
from conductor import worktree_reap


def _git_ok(where: Path, *args: str) -> str:
    """One git invocation inside ``where``; nonzero exits fail the test."""
    done = subprocess.run(
        ["git", *args], cwd=where, capture_output=True, text=True, check=False
    )
    assert done.returncode == 0, f"git {' '.join(args)} in {where}: {done.stderr}"
    return done.stdout.strip()


def _seeded_repo(
    parent: Path,
    name: str,
    *,
    branch: str,
    integration: str | None = None,
) -> Path:
    """A repo with one pushed commit on ``branch`` and an optional conductor table."""
    origin = parent / f"{name}-origin.git"
    _git_ok(parent, "init", "--quiet", "--bare", "-b", branch, str(origin))
    repo = parent / name
    _git_ok(parent, "init", "--quiet", "-b", branch, str(repo))
    _git_ok(repo, "config", "user.email", "hygiene@example.invalid")
    _git_ok(repo, "config", "user.name", "hygiene")
    (repo / "seed.txt").write_text("seed\n", encoding="utf-8")
    _git_ok(repo, "add", "seed.txt")
    _git_ok(repo, "commit", "--quiet", "-m", "seed")
    _git_ok(repo, "remote", "add", "origin", str(origin))
    _git_ok(repo, "push", "--quiet", "origin", branch)
    if integration is not None:
        (repo / "pyproject.toml").write_text(
            f'[tool.conductor]\nintegration_branch = "{integration}"\n',
            encoding="utf-8",
        )
    return repo


def _commit(repo: Path, name: str) -> None:
    (repo / name).write_text(f"{name}\n", encoding="utf-8")
    _git_ok(repo, "add", name)
    _git_ok(repo, "commit", "--quiet", "-m", name)


def _lineless_repo(parent: Path, name: str) -> Path:
    """A repo on branch ``trunk`` with no remote and no conductor table.

    Nothing here names an integration line -- not ``origin/main`` nor a local
    ``main`` -- so containment has nothing to be judged against and the checks
    must say ``unknown`` rather than guess.
    """
    repo = parent / name
    _git_ok(parent, "init", "--quiet", "-b", "trunk", str(repo))
    _git_ok(repo, "config", "user.email", "hygiene@example.invalid")
    _git_ok(repo, "config", "user.name", "hygiene")
    (repo / "tracked.txt").write_text("tracked\n", encoding="utf-8")
    _git_ok(repo, "add", "tracked.txt")
    _git_ok(repo, "commit", "--quiet", "-m", "tracked")
    return repo


# ---------------------------------------------------------------------------
# defect 1: the integration line is resolved, never the origin/master literal
# ---------------------------------------------------------------------------


def test_default_integration_ref_reads_the_configured_branch(tmp_path: Path) -> None:
    repo = _seeded_repo(tmp_path, "cfg", branch="master", integration="master")
    assert worktree_reap.default_integration_ref(repo) == "origin/master"


def test_default_integration_ref_falls_back_to_the_local_head_symref(
    tmp_path: Path,
) -> None:
    repo = _seeded_repo(tmp_path, "symref", branch="master")
    _git_ok(
        repo,
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/master",
    )
    assert worktree_reap.default_integration_ref(repo) == "origin/master"


def test_default_integration_ref_asks_the_remote_when_nothing_local_is_bound(
    tmp_path: Path,
) -> None:
    """A clone of an empty bare repo never binds origin/HEAD locally; the remote
    itself still advertises its line, and that answers."""
    repo = _seeded_repo(tmp_path, "advertised", branch="master")
    assert worktree_reap.default_integration_ref(repo) == "origin/master"


def test_default_integration_ref_prefers_origin_over_the_local_branch(
    tmp_path: Path,
) -> None:
    repo = _seeded_repo(tmp_path, "plain", branch="main")
    _commit(repo, "unpushed.txt")
    assert worktree_reap.default_integration_ref(repo) == "origin/main"


def test_default_integration_ref_refuses_a_repo_with_no_line_at_all(
    tmp_path: Path,
) -> None:
    repo = _lineless_repo(tmp_path, "lineless")
    with pytest.raises(worktree_reap.ReapError, match="no integration line"):
        worktree_reap.default_integration_ref(repo)


def test_decide_judges_containment_against_the_resolved_line(tmp_path: Path) -> None:
    """The reap decision with no explicit ref must find a finished worktree on a
    repo whose line is ``main`` -- on main it judged against the ``origin/master``
    literal, the containment trigger never fired, and the tree read as "run is
    not over"."""
    repo = _seeded_repo(tmp_path, "reap", branch="main")
    done = tmp_path / "done-tree"
    _git_ok(repo, "worktree", "add", "--quiet", "-b", "topic/done", str(done), "HEAD")
    proc_root = tmp_path / "empty-proc"
    (proc_root / "4242").mkdir(parents=True)
    decisions = worktree_reap.decide(repo, current=repo, proc_root=proc_root)
    row = next(d for d in decisions if d.worktree.path.resolve() == done.resolve())
    assert row.eligible
    assert "contained in origin/main" in row.reasons[0]


def test_reap_preview_survives_a_main_line_repo(tmp_path: Path) -> None:
    repo = _seeded_repo(tmp_path, "preview", branch="main")
    done = tmp_path / "preview-tree"
    _git_ok(repo, "worktree", "add", "--quiet", "-b", "topic/over", str(done), "HEAD")
    rows = wh.reap_preview(repo)
    assert rows and "contained in origin/main" in rows[0]["reason"]


def test_live_ref_or_default_resolves_without_a_literal(tmp_path: Path) -> None:
    repo = _seeded_repo(tmp_path, "live", branch="master")
    assert wh._live_ref_or_default(repo) == "origin/master"
    with pytest.raises(wh.HygieneError, match="no integration line"):
        wh._live_ref_or_default(_lineless_repo(tmp_path, "lineless"))


# ---------------------------------------------------------------------------
# defect 2: ROOT is the repository root, not the package parent
# ---------------------------------------------------------------------------


def test_root_is_the_repository_root() -> None:
    """``parents[1]`` of the module is ``src/``; the report needs the checkout."""
    assert wh.ROOT == Path(__file__).resolve().parents[2]
    assert (wh.ROOT / ".git").exists()


def test_manifest_state_finds_the_configured_registry() -> None:
    """On main this raised ``registry not found``: REGISTRY was joined to src/."""
    orphans, dangling = wh.manifest_state()
    assert isinstance(orphans, list) and isinstance(dangling, list)


def test_idle_claims_read_paths_from_the_repository_root(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A live-but-quiet claim over ``tracked.txt`` is idle -- unless claim paths
    are joined to src/, where nothing is ever found.

    The claim is created 100 minutes ago (inside the 2h active clamp, so it is
    still live) and the file has not been touched since: that is the idle window
    this check exists to catch, and on main the mtime was read from
    ``src/tracked.txt`` so the claim could never be quiet-on-record.
    """
    import os
    from datetime import UTC, datetime, timedelta

    from conductor.candidate_review.ownership import create_claim

    repo = _seeded_repo(tmp_path, "claims", branch="main")
    create_claim(
        repo,
        owner="hygiene-test",
        paths=["tracked.txt"],
        justification="slice I defect test",
        expected_minutes=15,
        max_minutes=115,
    )
    quiet = datetime.now(UTC) - timedelta(minutes=100)
    stale = repo / "tracked.txt"
    stale.write_text("aged\n", encoding="utf-8")
    os.utime(stale, (quiet.timestamp(), quiet.timestamp()))
    store = repo / ".git" / "governance" / "ownership-claims.json"
    payload = json.loads(store.read_text(encoding="utf-8"))
    from conductor.candidate_review.model import sha256_json

    row = payload["claims"][0]
    row["created_at"] = quiet.isoformat()
    fields = {
        key: row[key]
        for key in ("owner", "paths", "justification", "created_at", "expires_at")
    }
    if row.get("expected_at") is not None:
        fields["expected_at"] = row["expected_at"]
    row["claim_id"] = "claim-" + sha256_json(fields)[:20]
    store.write_text(json.dumps(payload), encoding="utf-8")

    monkeypatch.setattr(wh, "ROOT", repo)
    idle = wh.idle_claims()
    assert idle and idle[0]["claim_id"] == row["claim_id"]
    assert idle[0]["idle_minutes"] == "100"


# ---------------------------------------------------------------------------
# defect 3: the claim query runs in-process
# ---------------------------------------------------------------------------


def test_claim_checks_never_spawn_a_second_interpreter(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """``_claim_store`` re-execed ``.venv/bin/python -m conductor...cli claims``
    (relative to src/, which alone broke it); both checks now read the store
    in-process. Reading the store may still run git (the claim store lives in
    the git common dir) -- only an interpreter spawn is a failure."""
    import conductor.workspace_hygiene as fresh_wh

    repo = _seeded_repo(tmp_path, "inproc", branch="main")
    real_run = fresh_wh.subprocess.run

    def no_interpreter(*args: object, **kwargs: object) -> object:
        argv = args[0] if args and isinstance(args[0], list) else kwargs.get("args")
        rendered = " ".join(str(part) for part in argv or [])
        assert "-m" not in rendered and "python" not in rendered, (
            f"claim checks must not spawn an interpreter: {rendered}"
        )
        return real_run(*args, **kwargs)

    monkeypatch.setattr(fresh_wh, "ROOT", repo)
    monkeypatch.setattr(fresh_wh.subprocess, "run", no_interpreter)
    assert fresh_wh.expired_claims() == []
    assert fresh_wh.idle_claims() == []


# ---------------------------------------------------------------------------
# the SessionStart inject line
# ---------------------------------------------------------------------------


def test_exposure_line_reports_the_cheap_counts(tmp_path: Path) -> None:
    repo = _seeded_repo(tmp_path, "line", branch="main")
    assert wh.exposure_line(repo) == (
        "EXPOSED: 0 local-only commit(s), 0 stale dirty file(s), "
        "0 finished worktree(s) to remove, branches skipped (needs gh). "
        "python -m conductor.workspace_hygiene"
    )


def test_exposure_line_says_unknown_when_there_is_no_integration_line(
    tmp_path: Path,
) -> None:
    repo = _lineless_repo(tmp_path, "lineless")
    assert wh.exposure_line(repo) == (
        "EXPOSED: 1 local-only commit(s), 0 stale dirty file(s), "
        "unknown finished worktree(s) to remove, branches skipped (needs gh). "
        "python -m conductor.workspace_hygiene"
    )
