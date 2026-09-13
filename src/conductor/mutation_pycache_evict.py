"""Per-child eviction of mutated sources' cached bytecode, at pytest startup.

fest's subprocess backend builds and launches each mutant's pytest command
itself, so no launcher of ours sits between the mutant's rewrite and the child
that imports it -- the only hook that does is pytest's own plugin loading:
``PYTEST_ADDOPTS="-p conductor.mutation_pycache_evict"`` imports this module
before collection imports anything under test. When the adapter's environment
tells this process where the run's cache scratch is and which sources the run
mutates, importing this module deletes those sources' cached bytecode under
the run-private prefix, so the first import of a mutated file compiles the
bytes fest just wrote, never a same-size, same-second predecessor.

This module deliberately does not import `conductor.bytecode_isolation`. It
runs inside the very children whose modules are mutated, so a mutant that
breaks the shared helpers must not break the eviction with it: the plugin
would die at startup, pytest would write no report, and an engine that grades
by exit code would record a kill for which not one test ran. The small cache
mapping below is a shadow copy of the one in `bytecode_isolation`, kept apart
for the same reason a checksum's reimplementation is.

Eviction failure fails closed on the whole run-private prefix: deleting the
entire prefix tree makes every child of the run recompile, which is slower
and always honest. The deletion is bounded -- the scratch must carry the run
marker beside its ``pycache`` tree and must not be the filesystem root, the
home directory, the repository root or any parent of the working directory;
a directory that fails those checks raises instead of dying, because a
directory the run did not create is not one its failure handling may delete.
Only if even the bounded deletion fails does the import raise -- a cache this
process cannot control is not one it can grade against.

Imported anywhere else -- as a library, or in a pytest no engine launched --
the variables are absent and the import does nothing.
"""

from __future__ import annotations

import importlib.util
import os
import shutil
import sys
from pathlib import Path

PLUGIN_NAME = "conductor.mutation_pycache_evict"
SCRATCH_ENV = "CONDUCTOR_PYCACHE_EVICT_SCRATCH"
SOURCES_ENV = "CONDUCTOR_PYCACHE_EVICT_SOURCES"

# Plain, `.opt-1` and `.opt-2`: every compilation a child can hold for one
# source.
_OPTIMIZATIONS = ("", "1", "2")

# The marker `bytecode_isolation.isolated_python_env` writes into every run
# scratch it creates. Kept as a local literal for the same reason
# `_cache_paths` is a local copy: this module must not import the module whose
# mutants it survives. `RUN_MARKER_NAME` there and this literal are pinned
# equal by a test.
_RUN_MARKER = ".bytecode-isolation-run"


def _cache_paths(source: str, prefix: Path) -> list[Path]:
    """The cache files a prefix-using interpreter may hold for `source`.

    The interpreter's own mapping (a copy here would drift), via the same
    ``sys.pycache_prefix`` switch ``bytecode_isolation`` uses -- see the
    module docstring for why this is a local copy and not an import.
    """

    absolute = Path(source).resolve()
    previous = sys.pycache_prefix
    try:
        sys.pycache_prefix = str(prefix)
        return [
            Path(importlib.util.cache_from_source(str(absolute), optimization=tag))
            for tag in _OPTIMIZATIONS
        ]
    finally:
        sys.pycache_prefix = previous


def evict_now() -> list[Path]:
    """Delete the mutated sources' caches named by the environment, if any.

    Both variables must be present: sources without a scratch name caches this
    process cannot locate, and refusing to guess is cheaper than evicting the
    wrong tree.
    """

    scratch = os.environ.get(SCRATCH_ENV)
    sources = os.environ.get(SOURCES_ENV)
    if not scratch or not sources:
        return []
    try:
        prefix = (Path(scratch) / "pycache").resolve()
        caches = [
            cache
            for source in sources.split(os.pathsep)
            for cache in _cache_paths(source, prefix)
            if cache.is_file()
        ]
        for cache in caches:
            cache.unlink()
        return caches
    except Exception:
        # Never take the child down at startup: an engine that grades by exit
        # code reads a startup crash as a kill, with no test having run. The
        # run's whole scratch dies instead, and every child of the run
        # recompiles -- but only a scratch this run actually created: the
        # deletion is bounded by `_refuses_to_delete`, and a directory that
        # fails those checks raises instead (a directory the run did not
        # create is not one its failure handling may remove). A deletion
        # failure still raises -- a cache this process cannot control is not
        # one it can grade against.
        _fail_closed_delete(scratch)
        return []


def _fail_closed_delete(scratch: str) -> None:
    """Delete the whole run scratch, or refuse loud, never half-way."""

    reason = _refuses_to_delete(Path(scratch))
    if reason:
        raise RuntimeError(f"refusing to fail closed on {scratch}: {reason}")
    shutil.rmtree(scratch)


def _refuses_to_delete(path: Path) -> str | None:
    """Why `path` may not be deleted by the fail-closed handler, if anything.

    A run scratch is a directory the launcher created: it is none of the
    places a mistake could cost real data -- the working directory itself,
    the filesystem root, the home directory, the repository checkout this
    process runs in, or any directory on the path to the working directory --
    and it carries the run marker beside its `pycache/` tree. The identity
    checks run first on purpose: a forbidden path is refused for what it is,
    whatever it happens to contain. Anything else still dies: the fail-closed
    deletion is what makes an eviction fault honest.
    """

    resolved = path.resolve()
    cwd = Path.cwd().resolve()
    if resolved == cwd:
        return f"{resolved} is the working directory itself"
    if resolved == Path(resolved.anchor):
        return f"{resolved} is the filesystem root"
    if resolved == Path.home().resolve():
        return f"{resolved} is the home directory"
    repo_root = _repo_root()
    if repo_root is not None and resolved == repo_root:
        return f"{resolved} is the repository root"
    if resolved in cwd.parents:
        return f"{resolved} is a parent of the working directory {cwd}"
    if not (resolved / "pycache").exists():
        return f"{resolved}/pycache does not exist; not a run scratch"
    if not (resolved / _RUN_MARKER).exists():
        return f"{resolved}/{_RUN_MARKER} is absent; not a run scratch"
    return None


def _repo_root() -> Path | None:
    """The nearest enclosing `.git` of the working directory, if any."""

    current = Path.cwd().resolve()
    for candidate in (current, *current.parents):
        if (candidate / ".git").exists():
            return candidate
    return None


# At pytest startup: plugin import happens before conftest and test modules,
# which is the entire point -- eviction must precede the first import of a
# mutated file.
evict_now()
