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
and always honest. Only if even that deletion fails does the import raise --
a cache this process cannot control is not one it can grade against.

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
    prefix = (Path(scratch) / "pycache").resolve()
    try:
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
        # prefix tree dies instead, and every child of the run recompiles.
        # A deletion failure here still raises -- that cache is out of
        # control, and grading against it would be a verdict of luck.
        shutil.rmtree(prefix)
        return []


# At pytest startup: plugin import happens before conftest and test modules,
# which is the entire point -- eviction must precede the first import of a
# mutated file.
evict_now()
