from __future__ import annotations

import os
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path

import pytest

from conductor.bytecode_isolation import (
    RUN_MARKER_NAME,
    cache_paths_for,
    evict_mutated_caches,
    isolated_python_env,
    scratch_root_for,
)

_ISOLATION_VARS = ("PYTHONDONTWRITEBYTECODE", "PYTHONPYCACHEPREFIX")


def _plain_env() -> dict[str, str]:
    """The host environment minus any isolation the harness itself runs under."""

    return {
        key: value
        for key, value in os.environ.items()
        if key not in _ISOLATION_VARS
    }


def _child(env: Mapping[str, str], cwd: Path, code: str = "import m; print(m.f())") -> str:
    """Run `code` in a child interpreter and report its stdout."""

    completed = subprocess.run(
        [sys.executable, "-c", code],
        cwd=cwd,
        env=dict(env),
        capture_output=True,
        text=True,
        timeout=120,
        check=True,
    )
    return completed.stdout.strip()


def _pinned_mtime_write(source: Path, text: str, before: os.stat_result) -> None:
    """Rewrite `source` the way an engine applies a mutant: keep time and size.

    Keeping the file's original atime/mtime (nanosecond resolution pinned back
    onto the new file) and matching the old byte count is exactly the rewrite
    CPython's whole-second mtime + size cache validation cannot see.
    """

    source.write_text(text, encoding="utf-8")
    os.utime(source, ns=(before.st_atime_ns, before.st_mtime_ns))


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


def test_the_env_carries_base_plus_the_run_private_prefix(tmp_path: Path) -> None:
    """`base` survives whole, and the no-write kill-switch is dropped from it.

    Caching unmutated modules is the point of the scheme, so an inherited
    `PYTHONDONTWRITEBYTECODE` in `base` must not survive into the child: the
    run would quietly go back to recompiling every module of every child.
    """

    base = {"KEEP": "yes", "PYTHONDONTWRITEBYTECODE": "1"}
    env = isolated_python_env(base, tmp_path / "scratch")

    assert env["KEEP"] == "yes"
    assert "PYTHONDONTWRITEBYTECODE" not in env
    assert env["PYTHONPYCACHEPREFIX"] == str(tmp_path / "scratch" / "pycache")
    assert base["PYTHONDONTWRITEBYTECODE"] == "1", "base must not be mutated"


def test_the_scratch_is_marked_the_moment_it_is_created(tmp_path: Path) -> None:
    """The fail-closed deletion in the engines' plugin refuses unmarked trees.

    `isolated_python_env` is where every run scratch is born, so it is where
    the marker lands -- a scratch that exists without one was not created by
    this scheme, and the plugin that would delete it on eviction trouble
    must refuse exactly that case.
    """

    isolated_python_env({}, tmp_path / "scratch")
    marker = tmp_path / "scratch" / RUN_MARKER_NAME
    assert marker.is_file()
    # The marker is a license, never data: an empty file, and nothing else.
    assert marker.read_text(encoding="utf-8") == ""


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


def test_a_second_mutant_of_the_same_file_never_reads_the_firsts_bytecode(
    tmp_path: Path,
) -> None:
    """Two same-size rewrites inside one mtime second, one run-private prefix.

    The first child compiles the unmutated file into the prefix. The engine
    then applies a mutant -- same size, mtime pinned to the same second -- and
    launches the next child through `isolated_python_env` with that file named
    as mutated. Without the per-launch eviction, CPython's (mtime, size)
    validation accepts the first child's cache and the second mutant is graded
    against the first one's bytes; the eviction is what makes this print 3.
    """

    source = tmp_path / "m.py"
    source.write_text("def f(): return 1\n", encoding="utf-8")
    before = source.stat()
    plain = _plain_env()
    scratch = tmp_path / "scratch"

    first = isolated_python_env(plain, scratch, mutated_paths=[source])
    assert _child(first, tmp_path) == "1"

    _pinned_mtime_write(source, "def f(): return 3\n", before)
    second = isolated_python_env(plain, scratch, mutated_paths=[source])
    assert _child(second, tmp_path) == "3"


def test_an_unmutated_module_is_served_from_the_prefix_on_the_second_child(
    tmp_path: Path,
) -> None:
    """Everything but the mutated file compiles once per run, not per child.

    Two children run against the same scratch with `m` named as mutated both
    times; `n` is never mutated. After the first child, `n`'s `.pyc` in the
    prefix is valid for every later child of the run: the file count after the
    second child equals the count after the first (nothing new was compiled),
    and `n`'s cache entry is the very same inode with the very same mtime --
    served from the prefix, not rewritten into it.
    """

    (tmp_path / "n.py").write_text("def g(): return 7\n", encoding="utf-8")
    source = tmp_path / "m.py"
    source.write_text("def f(): return 1\n", encoding="utf-8")
    before = source.stat()
    plain = _plain_env()
    scratch = tmp_path / "scratch"
    code = "import m, n; print(m.f(), n.g())"

    first = isolated_python_env(plain, scratch, mutated_paths=[source])
    assert _child(first, tmp_path, code) == "1 7"
    n_cache = cache_paths_for(tmp_path / "n.py", scratch / "pycache")[0]
    assert n_cache.is_file(), "the first child must have cached n in the prefix"
    n_stat = n_cache.stat()
    count = len(list((scratch / "pycache").rglob("*.pyc")))

    _pinned_mtime_write(source, "def f(): return 4\n", before)
    second = isolated_python_env(plain, scratch, mutated_paths=[source])
    assert _child(second, tmp_path, code) == "4 7"

    assert len(list((scratch / "pycache").rglob("*.pyc"))) == count
    again = n_cache.stat()
    assert (again.st_ino, again.st_mtime_ns) == (n_stat.st_ino, n_stat.st_mtime_ns)


def test_eviction_refuses_to_leave_the_runs_own_scratch(tmp_path: Path) -> None:
    """A cache path that escapes the run's prefix fails loud instead of unlinking.

    The eviction deletes files to buy immunity, so a computed path that does
    not live under the run's own prefix is refused before anything is removed.
    A symlink planted in the mirrored source directory makes the computed path
    resolve outside the prefix -- the one way the mapping can be led astray.
    """

    source = tmp_path / "proj" / "m.py"
    source.parent.mkdir()
    source.write_text("x = 1\n", encoding="utf-8")
    run = tmp_path / "run"
    outside = tmp_path / "outside"
    outside.mkdir()

    victim = cache_paths_for(source, scratch_root_for(run) / "pycache")[0]
    mirrored = victim.parent.parent
    mirrored.parent.mkdir(parents=True)
    mirrored.symlink_to(outside)
    victim.parent.mkdir()
    victim.write_bytes(b"stale")

    with pytest.raises(RuntimeError, match="not under the run's cache prefix"):
        evict_mutated_caches([source], scratch_root_for(run))
    assert victim.is_file(), "a refused eviction must not delete anything"


def test_the_runs_scratch_root_is_one_pinned_name(tmp_path: Path) -> None:
    """The launcher, the plugin and attribution must agree on where caches live.

    A renamed scratch silently forks a run's caches: whoever computes the old
    name stops finding them, whoever computes the new one starts from an
    empty prefix. One literal, shared by every consumer.
    """

    assert scratch_root_for(tmp_path) == tmp_path / ".bytecode-isolation"


def test_eviction_reports_exactly_the_files_it_removed(tmp_path: Path) -> None:
    """The return value is the receipt of what died, nothing more.

    Callers (the plugin's fault accounting among them) read this list to know
    a cache was actually dropped -- a lie here reads as immunity.
    """

    source = tmp_path / "m.py"
    source.write_text("x = 1\n", encoding="utf-8")
    scratch = tmp_path / "scratch"
    caches = cache_paths_for(source, scratch / "pycache")
    for cache in caches:
        cache.parent.mkdir(parents=True, exist_ok=True)
        cache.write_bytes(b"stale")

    assert evict_mutated_caches([source], scratch) == caches
    assert not any(cache.exists() for cache in caches)
