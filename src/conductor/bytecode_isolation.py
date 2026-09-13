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

Two exports close it for every child interpreter. ``PYTHONDONTWRITEBYTECODE``
stops cache writes anywhere, and ``PYTHONPYCACHEPREFIX`` moves the cache to a
fresh per-run scratch directory: with a prefix set, CPython stops reading
beside-source ``__pycache__`` entirely, so even a stale cache that already
exists cannot be read. The price is one compile per module per run, which the
campaign receipts report as wall time.
"""

from __future__ import annotations

import os
from collections.abc import Mapping
from pathlib import Path


def isolated_python_env(base: Mapping[str, str], scratch: Path) -> dict[str, str]:
    """`base` plus the two exports that make a child compile from source.

    `scratch` is created and must be writable -- a run that cannot have its
    own cache directory fails rather than falling back to shared caches,
    because the fallback is the stale-read this module exists to prevent.
    Callers give each run a fresh `scratch`, so the prefix starts empty and no
    child can find a cache an earlier run left.
    """

    prefix = scratch / "pycache"
    try:
        prefix.mkdir(parents=True, exist_ok=True)
        if not os.access(prefix, os.W_OK):
            raise OSError(f"not writable: {prefix}")
    except OSError as exc:
        raise RuntimeError(
            f"bytecode isolation scratch {scratch} is not usable: {exc}"
        ) from exc
    return {
        **base,
        "PYTHONDONTWRITEBYTECODE": "1",
        "PYTHONPYCACHEPREFIX": str(prefix),
    }
