"""Where a hook body is found, and which interpreter runs it.

Both answers are load-bearing for `conductor init`, which scaffolds this
tooling into a project that has no `tooling/` tree of its own: the bodies then
ship inside the installed package, and the dispatcher has to find them there
while a checkout of the tooling itself keeps running its own. Neither rule had
a test, and both fail in a way that only shows up in the foreign project --
never in the monorepo where every path happens to exist.
"""

from __future__ import annotations

import sys
from pathlib import Path

from tooling.hooks.dispatch import paths

RELATIVE = "tooling/hooks/agent/crg_gate.py"


def test_the_projects_own_copy_wins(tmp_path: Path) -> None:
    """A checkout of the tooling must run the bodies it is editing.

    Preferring the installed package would make every hook change invisible
    until the package was reinstalled, which is the slowest possible feedback
    loop for the files that gate every write.
    """

    body = tmp_path / RELATIVE
    body.parent.mkdir(parents=True)
    body.write_text("# project copy\n", encoding="utf-8")
    assert paths.body_path(tmp_path, RELATIVE) == body


def test_the_installed_package_answers_when_the_project_has_none(
    tmp_path: Path,
) -> None:
    """A foreign project scaffolded by `conductor init` has no tooling/ tree.

    Returning the missing project path would leave every hook pointing at a
    file that does not exist, which is the whole reason for the fallback.
    """

    found = paths.body_path(tmp_path, RELATIVE)
    assert found == paths.TOOLING_ROOT / RELATIVE
    assert not found.is_relative_to(tmp_path)


def test_a_directory_is_not_a_body(tmp_path: Path) -> None:
    """The project copy has to be a file, not merely an existing path.

    `tooling/hooks/agent/` exists as a directory in plenty of trees; treating
    that as the body would shadow the installed file with something unrunnable.
    """

    (tmp_path / RELATIVE).mkdir(parents=True)
    assert paths.body_path(tmp_path, RELATIVE) == paths.TOOLING_ROOT / RELATIVE


def test_the_tooling_root_holds_the_tooling_package() -> None:
    """TOOLING_ROOT is the directory `tooling/` sits in, in either layout.

    It is computed by counting parents from this file, so a module moved one
    level deeper silently reroutes every fallback body to a sibling directory.
    """

    assert (paths.TOOLING_ROOT / "tooling" / "hooks" / "dispatch").is_dir()


def test_the_interpreter_bin_does_not_follow_the_venv_symlink(
    monkeypatch, tmp_path: Path
) -> None:
    """The venv's own bin, not the base interpreter its symlink points at.

    A venv's `bin/python3` is a symlink out to the system interpreter, which
    carries no `conductor`. Following it makes every shell body that runs
    `python3` import nothing, and only in the environments that matter.
    """

    base = tmp_path / "usr/bin"
    base.mkdir(parents=True)
    (base / "python3.12").write_text("", encoding="utf-8")
    venv = tmp_path / "venv/bin"
    venv.mkdir(parents=True)
    link = venv / "python3"
    link.symlink_to(base / "python3.12")

    monkeypatch.setattr(sys, "executable", str(link))
    assert paths.interpreter_bin() == str(venv)


def _venv(root: Path) -> Path:
    """A checkout with its own interpreter, as `own_interpreter` expects to find it."""
    python = root / ".venv" / "bin" / "python"
    python.parent.mkdir(parents=True, exist_ok=True)
    python.write_text('#!/bin/sh\nexec /usr/bin/env python3 "$@"\n', encoding="utf-8")
    python.chmod(0o755)
    return python


def test_own_interpreter_names_the_checkouts_python_for_a_foreign_caller(
    tmp_path: Path,
) -> None:
    """The whole point: a caller on some other venv is told to switch.

    Asserted against two roots in one test, because the answer must be derived from
    `project_root` rather than from this file's location: two checkouts on one machine
    resolve to two different interpreters, which is what keeps a worktree from
    importing native extensions built for a different tree. Split across two tests the
    second one kills no mutant the first does not -- the value analysis classified it
    DELETE_CANDIDATE, and a nodeid that detects no failure is a check-box.
    """
    foreign = "/some/other/venv/bin/python"
    main, worktree = tmp_path / "main", tmp_path / "worktree"
    main_python, worktree_python = _venv(main), _venv(worktree)
    assert paths.own_interpreter(main, foreign) == main_python
    assert paths.own_interpreter(worktree, foreign) == worktree_python


def test_own_interpreter_is_none_when_the_caller_already_runs_it(
    tmp_path: Path,
) -> None:
    """No re-exec loop: the correct interpreter must not be told to switch to itself."""
    root = tmp_path / "checkout"
    python = _venv(root)
    assert paths.own_interpreter(root, str(python)) is None


def test_own_interpreter_resolves_before_comparing(tmp_path: Path) -> None:
    """A symlink to the checkout's python is the same interpreter, not a foreign one.

    Comparing the raw strings would re-exec every time a caller reached the venv
    through a link -- the loop the env guard exists to bound, fired on every hook.
    """
    root = tmp_path / "checkout"
    python = _venv(root)
    link = tmp_path / "link-to-python"
    link.symlink_to(python)
    assert paths.own_interpreter(root, str(link)) is None


def test_own_interpreter_is_none_when_the_checkout_has_no_venv(tmp_path: Path) -> None:
    """A worktree without a .venv has nothing to switch to; the caller warns."""
    root = tmp_path / "checkout"
    root.mkdir()
    assert paths.own_interpreter(root, "/some/other/venv/bin/python") is None
