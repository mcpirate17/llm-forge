"""Bytecode-cache isolation for child interpreters.

CPython accepts a ``__pycache__`` entry when the source's mtime and size match
what the entry recorded. A rewrite that changes no byte count and lands within
the same mtime second therefore leaves a stale ``.pyc`` that every later
interpreter keeps executing while the file on disk says something else. A
mutation engine is that case on purpose: it rewrites a file to apply a mutant
and immediately re-runs the suite against it, so the suite can grade the
unmutated bytecode and report a survivor (or a kill) that no source diff
explains. This happened on the retention campaign -- a spurious
``arithmetic_op`` survivor vanished the moment the caches were purged (PR #49).

The isolation is a run-private cache prefix plus per-launch eviction.
``PYTHONPYCACHEPREFIX`` points every child at ``<scratch>/pycache``, created
fresh per run by the caller (a disposable snapshot), and with a prefix set
CPython never reads beside-source caches -- so a stale one cannot be read even
where it already exists. ``PYTHONDONTWRITEBYTECODE`` is deliberately NOT set:
with it every child recompiled every module, which PR #51 measured at 2.6x
campaign wall time, while only the mutated file can ever be stale -- every
other module is byte-identical across the mutants of one run. Unmutated
modules therefore compile once into the prefix and are served from it after,
and each mutated file's cache is deleted before the child that must not trust
it launches (``evict_mutated_caches`` / ``isolated_python_env``), which
restores the same-second immunity for exactly the file that needs it.
"""

from __future__ import annotations

import os
import sys
from collections.abc import Iterable, Mapping
from pathlib import Path

# The three compilations a child can produce for one source: plain, and the
# ``.opt-1``/``.opt-2`` variants an optimized or assertion-stripping run may
# leave instead.
_CACHE_OPTIMIZATIONS = ("", "1", "2")

# Written into every scratch this module creates. The engines' pytest plugin
# fails closed by deleting the whole scratch, so it deletes only directories
# carrying this marker -- a directory the run did not create is not one its
# failure handling may remove.
RUN_MARKER_NAME = ".bytecode-isolation-run"


def scratch_root_for(cwd: Path) -> Path:
    """The run-private isolation root for children launched in ``cwd``.

    One name, used by the launcher (`mutation_engine_generated.run`), by the
    engines' pytest plugin and by attribution's per-mutant eviction, so every
    consumer of a run's caches agrees on where they live.
    """

    return cwd / ".bytecode-isolation"


def cache_paths_for(source: Path, prefix: Path) -> list[Path]:
    """Every ``.pyc`` a prefix-using interpreter may hold for ``source``.

    ``importlib.util.cache_from_source`` is the interpreter's own mapping (a
    copy would drift), and it honours ``sys.pycache_prefix`` by mirroring the
    absolute source path under the prefix; the switch is local to this call
    because the parent process itself runs without a prefix.
    """

    import importlib.util

    absolute = Path(source).resolve()
    root = prefix.resolve()
    previous = sys.pycache_prefix
    try:
        sys.pycache_prefix = str(root)
        return [
            Path(
                importlib.util.cache_from_source(
                    str(absolute), optimization=optimization
                )
            )
            for optimization in _CACHE_OPTIMIZATIONS
        ]
    finally:
        sys.pycache_prefix = previous


def evict_mutated_caches(
    mutated_paths: Iterable[Path | str], scratch: Path
) -> list[Path]:
    """Delete the cached bytecode of ``mutated_paths`` under the run's prefix.

    Called before the child that is about to read those sources launches --
    from ``isolated_python_env`` at the launcher, from the engines' pytest
    plugin inside children the launcher cannot hook, and from attribution
    around every re-applied mutant. Every deleted file must live under the
    run's own ``<scratch>/pycache``: deletion is how this module buys immunity,
    so a computed path that escapes the scratch fails loud instead of
    unlinking something the run never created.
    """

    prefix = (scratch / "pycache").resolve()
    removed: list[Path] = []
    for source in mutated_paths:
        for cache in cache_paths_for(Path(source), prefix):
            resolved = cache.resolve()
            if resolved != prefix and prefix not in resolved.parents:
                raise RuntimeError(
                    f"refusing to evict {cache}: it is not under the run's "
                    f"cache prefix {prefix}"
                )
            if resolved.exists():
                resolved.unlink()
                removed.append(resolved)
    return removed


def isolated_python_env(
    base: Mapping[str, str], scratch: Path, *, mutated_paths: Iterable[Path | str] = ()
) -> dict[str, str]:
    """``base`` bound to the run's private bytecode cache, mutated sources evicted.

    ``scratch`` is created and must be writable -- a run that cannot have its
    own cache directory fails rather than falling back to shared caches,
    because the fallback is the stale read this module exists to prevent.
    Callers hand each run a fresh ``scratch`` (a disposable snapshot's own
    tree), so the prefix starts empty and no child can find a cache an earlier
    run left. An inherited ``PYTHONDONTWRITEBYTECODE`` is dropped rather than
    honoured: a host that ran the engine under it would silently pay the
    recompile-everything cost this scheme removes. (A flag in the host's own
    ``os.environ`` still wins through the launch merge -- slower, never less
    isolated, because the prefix is what buys the stale-read immunity.)

    The scratch is marked with ``RUN_MARKER_NAME`` the moment it is created:
    the plugin that fails closed by deleting the scratch refuses any directory
    without it.
    """

    prefix = scratch / "pycache"
    try:
        prefix.mkdir(parents=True, exist_ok=True)
        if not os.access(prefix, os.W_OK):
            raise OSError(f"not writable: {prefix}")
        (scratch / RUN_MARKER_NAME).write_text("", encoding="utf-8")
    except OSError as exc:
        raise RuntimeError(
            f"bytecode isolation scratch {scratch} is not usable: {exc}"
        ) from exc
    evict_mutated_caches(mutated_paths, scratch)
    environment = dict(base)
    environment.pop("PYTHONDONTWRITEBYTECODE", None)
    environment["PYTHONPYCACHEPREFIX"] = str(prefix)
    return environment
