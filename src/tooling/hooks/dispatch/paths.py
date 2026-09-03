"""Where hook bodies live: this checkout of the tooling, or the installed package.

``registry.py`` names every body by its tooling-relative path
(``tooling/hooks/agent/crg_gate.py``). In the monorepo that path exists under the
project root; in a foreign project scaffolded by ``conductor init`` the project has
no ``tooling/`` tree and the bodies ship inside the installed ``tooling`` package.
The project copy wins when both exist, so a checkout of the tooling itself always
runs its own bodies.
"""

from __future__ import annotations

import sys
from pathlib import Path

# <root>/tooling/hooks/dispatch/paths.py -> <root>; in site-packages, <root> is the
# site directory holding the installed ``tooling`` package.
TOOLING_ROOT: Path = Path(__file__).resolve().parents[3]


def body_path(project_root: Path, relative: str) -> Path:
    """The file for a registry body path: the project's copy, else the package's."""
    candidate = project_root / relative
    if candidate.is_file():
        return candidate
    return TOOLING_ROOT / relative


def interpreter_bin() -> str:
    """The directory of the running interpreter, so ``python3`` in a shell body
    resolves to the environment that carries ``conductor``."""
    # ``sys.executable`` is absolute already; resolving it would follow a venv's
    # symlink out to the base interpreter, which carries no tooling at all.
    return str(Path(sys.executable).parent)
