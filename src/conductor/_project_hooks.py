"""Project-specific hooks that ``conductor`` needs but must not own.

This is the one place in ``conductor/`` that knows a host project exists. The host's
pytest path guard (the plugin that fails a test reading outside the exported tree and
skips one reading a declared-absent artifact) is project knowledge, not generic
tooling: it hardcodes the host's sealed-artifact lists and allowlist roots. So it is
not imported here; it is *configured*:

``CONDUCTOR_PROJECT_TEST_PLUGIN``
    Dotted ``module:function`` naming a callable that takes a ``pytest.Config``.
    Unset: the default below (this repo's guard). Empty string: no guard at all,
    which is what a standalone install without a host project uses. Set to something
    that does not import or lacks the function: an error naming the variable --
    a guard that silently fails to register is exactly the defect this seam exists
    to prevent.

The default is a string resolved through ``importlib`` at call time, never an import
statement, so ``conductor/`` carries no import edge to the host package and the
boundary contract (``conductor/tooling_boundary.py``) can allowlist the one string.
"""

from __future__ import annotations

import importlib
import os
from collections.abc import Callable
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    import pytest

PLUGIN_ENV = "CONDUCTOR_PROJECT_TEST_PLUGIN"
DEFAULT_TEST_PLUGIN = "research.tests._path_guard:register"


def resolve_test_plugin(spec: str | None) -> Callable[[Any], None] | None:
    """Turn a ``module:function`` spec into the callable, or ``None`` for no guard."""
    if spec is None:
        spec = DEFAULT_TEST_PLUGIN
    if spec == "":
        return None
    module_name, sep, function_name = spec.partition(":")
    if not sep or not module_name or not function_name:
        raise ValueError(f"{PLUGIN_ENV}={spec!r} is not a 'module:function' spec")
    try:
        module = importlib.import_module(module_name)
    except ImportError as exc:
        raise ImportError(
            f"{PLUGIN_ENV}={spec!r}: cannot import {module_name}: {exc}"
        ) from exc
    try:
        return getattr(module, function_name)
    except AttributeError as exc:
        raise ImportError(
            f"{PLUGIN_ENV}={spec!r}: {module_name} has no {function_name}"
        ) from exc


def register_test_path_guard(config: pytest.Config) -> None:
    """Register the configured host plugin on ``config``; a no-op when disabled."""
    plugin = resolve_test_plugin(os.environ.get(PLUGIN_ENV))
    if plugin is not None:
        plugin(config)
