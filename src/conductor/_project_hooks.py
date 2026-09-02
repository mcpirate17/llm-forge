"""Project-specific hooks that ``conductor`` needs but must not own.

This is the one place in ``conductor/`` that reaches into the host project's test
support. ``research/tests/_path_guard.py`` is the pytest plugin that fails a test which
reads outside the exported tree and skips one that reads a declared-absent artifact.
It was not moved into ``conductor`` because it is project knowledge, not generic
tooling: it imports ``research.tests._artifacts`` (``REPO_ROOT``, the exact
``UNSHIPPABLE_ARTIFACTS`` and ``SIZE_EXEMPT_ARTIFACTS`` lists of this repo's sealed
contracts) and hardcodes this project's allowlist roots (``.claude/``, ``.codex/``,
``.qwen/``, ``.grok/``, ``/mnt/data`` per KB-DATA-01). A standalone ``conductor``
package registers whatever guard its host provides here; a host without one leaves
``register_test_path_guard`` a no-op.
"""

from __future__ import annotations

import pytest


def register_test_path_guard(config: pytest.Config) -> None:
    """Register the host project's path guard plugin on ``config`` (idempotent)."""
    from research.tests._path_guard import register

    register(config)
