"""Focused tests for command_runner's {changed_files_file} substitution.

jscpd and vulture always scan the whole tree against a baseline instead of
operating on the ``{files}`` list directly, so their --changed-file(s)
attribution flag needs a path on disk rather than an inline argument list.
``_expand_command`` writes that list to ``ctx.runtime_dir`` on demand and
substitutes ``{changed_files_file}`` with its path; every other token keeps
behaving exactly as before.
"""

from __future__ import annotations

from pathlib import Path

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.command_runner import _expand_command
from conductor.candidate_review.model import Candidate


def _ctx(tmp_path: Path) -> ReviewContext:
    repo = tmp_path / "repo"
    snapshot = tmp_path / "snapshot"
    runtime_dir = tmp_path / "runtime"
    repo.mkdir()
    snapshot.mkdir()
    return ReviewContext(
        repo=repo,
        snapshot=snapshot,
        candidate=Candidate(
            kind="pr",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid="c" * 40,
            commit_oid="d" * 40,
            target_ref="refs/heads/main",
            changes=(),
        ),
        entries=(),
        policy=None,  # type: ignore[arg-type]
        surface="test",
        profile="fast",
        owner=None,
        runtime_dir=runtime_dir,
    )


def test_changed_files_file_token_is_written_and_substituted(tmp_path: Path) -> None:
    ctx = _ctx(tmp_path)
    files = ["conductor/a.py", "conductor/b.py"]

    command = _expand_command(
        ["vulture", "--changed-files-from", "{changed_files_file}"],
        ctx,
        files,
        check_id="vulture",
    )

    assert command[0] == "vulture"
    assert command[1] == "--changed-files-from"
    written = Path(command[2])
    assert written.parent == ctx.runtime_dir
    assert written.read_text(encoding="utf-8").splitlines() == files


def test_changed_files_file_is_empty_when_no_files_matched(tmp_path: Path) -> None:
    ctx = _ctx(tmp_path)

    command = _expand_command(
        ["vulture", "--changed-files-from", "{changed_files_file}"],
        ctx,
        [],
        check_id="vulture",
    )

    written = Path(command[2])
    assert written.read_text(encoding="utf-8") == ""


def test_no_changed_files_file_token_writes_no_scratch_file(tmp_path: Path) -> None:
    ctx = _ctx(tmp_path)

    command = _expand_command(
        ["{python}", "-m", "conductor.run_duplicate_audit", "--check"],
        ctx,
        ["conductor/a.py"],
        check_id="jscpd",
    )

    assert "{changed_files_file}" not in command
    assert not (ctx.runtime_dir / "changed-files-jscpd.txt").exists()


def test_files_token_still_expands_inline_as_before(tmp_path: Path) -> None:
    ctx = _ctx(tmp_path)

    command = _expand_command(
        ["cppcheck", "{files}"],
        ctx,
        ["conductor/a.py", "conductor/b.py"],
        check_id="cppcheck",
    )

    assert command == ["cppcheck", "conductor/a.py", "conductor/b.py"]


def test_changed_files_file_name_is_scoped_per_check_id(tmp_path: Path) -> None:
    ctx = _ctx(tmp_path)

    jscpd_command = _expand_command(
        ["jscpd-tool", "{changed_files_file}"], ctx, ["a.py"], check_id="jscpd"
    )
    vulture_command = _expand_command(
        ["vulture-tool", "{changed_files_file}"], ctx, ["b.py"], check_id="vulture"
    )

    assert jscpd_command[1] != vulture_command[1]
    assert Path(jscpd_command[1]).read_text(encoding="utf-8").strip() == "a.py"
    assert Path(vulture_command[1]).read_text(encoding="utf-8").strip() == "b.py"
