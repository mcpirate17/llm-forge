"""``python -m conductor.bootstrap <host-root>``: the platform's host bootstrap entry.

This is an alias onto :mod:`conductor.project_init` (``conductor init``), not a second
scaffolding engine. ``project_init`` already writes, idempotently and fail-loud,
everything a foreign project needs: the dispatcher's ``hooks`` block merged into
``.claude/settings.json``, the ``.claude/hooks/dispatch.py`` launcher (its shebang
pinned to the interpreter that has this package installed, so it never depends on an
in-tree ``tooling/`` directory), the code-review-graph MCP entry, a starter candidate
policy and mutation registry, a ``.github/workflows/weekly-audit.yml`` running this
package's own audits, a ``.claude/hooks/project/env.sh`` stub for repo-specific hook
defaults, and a warning (never a write) when the host's ``pyproject.toml`` is missing
the ``[tool.conductor]`` keys ``conductor.project_paths`` needs.

``conductor.bootstrap`` exists only so a host can invoke the platform's onboarding
command by the name this project advertises for it; a second implementation here would
duplicate ``project_init``'s settings/MCP merge, conflict handling and doctor run for
no behavioral difference. See ``conductor/project_init.py`` for the full contract,
``conductor/test_bootstrap.py`` for the alias and shim contract this module owns.
"""

from __future__ import annotations

from collections.abc import Sequence

from conductor.project_init import main as _init_main


def main(argv: Sequence[str] | None = None) -> int:
    return _init_main(argv)


if __name__ == "__main__":
    raise SystemExit(main())
