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

Imported anywhere else -- as a library, or in a pytest no engine launched --
the variables are absent and the import does nothing.
"""

from __future__ import annotations

import os
from pathlib import Path

from conductor.bytecode_isolation import evict_mutated_caches

PLUGIN_NAME = "conductor.mutation_pycache_evict"
SCRATCH_ENV = "CONDUCTOR_PYCACHE_EVICT_SCRATCH"
SOURCES_ENV = "CONDUCTOR_PYCACHE_EVICT_SOURCES"


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
    return evict_mutated_caches(sources.split(os.pathsep), Path(scratch))


# At pytest startup: plugin import happens before conftest and test modules,
# which is the entire point -- eviction must precede the first import of a
# mutated file.
evict_now()
