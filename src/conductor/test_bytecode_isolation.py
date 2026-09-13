from __future__ import annotations

import os
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path

import pytest

from conductor.bytecode_isolation import isolated_python_env

_ISOLATION_VARS = ("PYTHONDONTWRITEBYTECODE", "PYTHONPYCACHEPREFIX")


def _plain_env() -> dict[str, str]:
    """The host environment minus any isolation the harness itself runs under."""

    return {
        key: value
        for key, value in os.environ.items()
        if key not in _ISOLATION_VARS
    }


def _child(env: Mapping[str, str], cwd: Path) -> str:
    """Import `m` in a child interpreter and report what `m.f()` returned."""

    completed = subprocess.run(
        [sys.executable, "-c", "import m; print(m.f())"],
        cwd=cwd,
        env=dict(env),
        capture_output=True,
        text=True,
        timeout=120,
        check=True,
    )
    return completed.stdout.strip()


def test_a_same_second_edit_runs_stale_code_until_the_env_is_isolated(
    tmp_path: Path,
) -> None:
    """The trap, demonstrated, then closed -- the regression test for the fix.

    The first child imports `m` and leaves a beside-source `__pycache__` entry.
    The file is then rewritten to return 2 with the same byte count and its
    original mtime pinned, which is precisely the state a mutation engine
    creates when it applies a same-size mutant: CPython's (mtime, size)
    validation cannot distinguish the rewrite from the file the cache was
    built from. The plain-env child asserts the resulting `1` as
    documentation of the trap -- that is what the default validation does,
    not what anyone wants -- and the isolated child asserts `2`.
    """

    source = tmp_path / "m.py"
    source.write_text("def f(): return 1\n", encoding="utf-8")
    before = source.stat()
    plain = _plain_env()

    assert _child(plain, tmp_path) == "1"
    caches = list((tmp_path / "__pycache__").glob("m.*.pyc"))
    assert caches, "the plain-env child must have written a beside-source cache"

    source.write_text("def f(): return 2\n", encoding="utf-8")
    os.utime(source, ns=(before.st_atime_ns, before.st_mtime_ns))

    # The trap is real: this child executes the stale bytecode for `return 1`.
    assert _child(plain, tmp_path) == "1"

    isolated = isolated_python_env(plain, tmp_path / "scratch")
    assert _child(isolated, tmp_path) == "2"


def test_the_env_carries_base_plus_both_exports(tmp_path: Path) -> None:
    """`base` survives whole, and an isolation variable in `base` is overridden."""

    base = {"KEEP": "yes", "PYTHONDONTWRITEBYTECODE": "0"}
    env = isolated_python_env(base, tmp_path / "scratch")

    assert env["KEEP"] == "yes"
    assert env["PYTHONDONTWRITEBYTECODE"] == "1"
    assert env["PYTHONPYCACHEPREFIX"] == str(tmp_path / "scratch" / "pycache")
    assert base["PYTHONDONTWRITEBYTECODE"] == "0", "base must not be mutated"


def test_each_scratch_names_its_own_prefix(tmp_path: Path) -> None:
    """Two runs never share a cache directory -- that sharing is the bug."""

    one = isolated_python_env({}, tmp_path / "one")
    two = isolated_python_env({}, tmp_path / "two")

    assert one["PYTHONPYCACHEPREFIX"] != two["PYTHONPYCACHEPREFIX"]


def test_an_unusable_scratch_fails_loud(tmp_path: Path) -> None:
    """A file where the scratch directory must go is refused, not papered over.

    `chmod 0o500` cannot serve here: a root user passes any access check, so
    the deterministic unwritable case is a path whose parent is a plain file.
    """

    blocker = tmp_path / "not-a-directory"
    blocker.write_text("{}", encoding="utf-8")

    with pytest.raises(RuntimeError, match="is not usable"):
        isolated_python_env({}, blocker / "pycache")
