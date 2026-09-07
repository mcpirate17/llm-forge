"""Resolve optional project-owned pytest hooks without importing host packages.

``CONDUCTOR_PROJECT_TEST_PLUGIN`` takes precedence over the optional
``[tool.conductor.pytest].test_plugin`` value in the pytest root's ``pyproject.toml``.
An absent environment value and absent configuration select no hook. An explicit empty
value disables the hook; an explicit ``module:function`` value is imported at call time.
"""

from __future__ import annotations

import importlib
import os
import stat
import tomllib
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    import pytest

PLUGIN_ENV = "CONDUCTOR_PROJECT_TEST_PLUGIN"
_CONFIG_LIMIT = 64 * 1024
_CONFIG_KEY = "[tool.conductor.pytest].test_plugin"


def _config_error(path: Path, message: str) -> ValueError:
    return ValueError(f"{path}: {message}")


def _table(value: object, path: Path, label: str) -> dict[str, object] | None:
    if value is None:
        return None
    if not isinstance(value, dict):
        raise _config_error(path, f"{label} must be a table")
    return value


def _configured_test_plugin(config: pytest.Config) -> tuple[str | None, str]:
    """Read the optional project hook selector from pytest's trusted root path."""
    path = Path(config.rootpath) / "pyproject.toml"
    try:
        metadata = path.stat()
    except FileNotFoundError:
        return None, _CONFIG_KEY
    except OSError as exc:
        raise _config_error(path, f"cannot inspect configuration: {exc}") from exc
    if not stat.S_ISREG(metadata.st_mode):
        raise _config_error(path, "configuration must be a regular file")
    if metadata.st_size > _CONFIG_LIMIT:
        raise _config_error(path, "configuration exceeds 64 KiB")
    try:
        with path.open("rb") as stream:
            data = stream.read(_CONFIG_LIMIT + 1)
    except OSError as exc:
        raise _config_error(path, f"cannot read configuration: {exc}") from exc
    if len(data) > _CONFIG_LIMIT:
        raise _config_error(path, "configuration exceeds 64 KiB")
    try:
        payload = tomllib.loads(data.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
        raise _config_error(path, f"invalid TOML configuration: {exc}") from exc
    tool = _table(payload.get("tool"), path, "[tool]")
    conductor = _table(
        None if tool is None else tool.get("conductor"), path, "[tool.conductor]"
    )
    pytest_config = _table(
        None if conductor is None else conductor.get("pytest"),
        path,
        "[tool.conductor.pytest]",
    )
    if pytest_config is None or "test_plugin" not in pytest_config:
        return None, f"{path} {_CONFIG_KEY}"
    selector = pytest_config["test_plugin"]
    if not isinstance(selector, str):
        raise _config_error(path, f"{_CONFIG_KEY} must be a string")
    return selector, f"{path} {_CONFIG_KEY}"


def resolve_test_plugin(
    spec: str | None, *, source: str = PLUGIN_ENV
) -> Callable[[Any], None] | None:
    """Turn a ``module:function`` spec into a callable, or ``None`` for no hook."""
    if spec is None or spec == "":
        return None
    module_name, separator, function_name = spec.partition(":")
    if not separator or not module_name or not function_name:
        raise ValueError(f"{source}={spec!r} is not a 'module:function' spec")
    try:
        module = importlib.import_module(module_name)
    except ImportError as exc:
        raise ImportError(
            f"{source}={spec!r}: cannot import {module_name}: {exc}"
        ) from exc
    try:
        plugin = getattr(module, function_name)
    except AttributeError as exc:
        raise ImportError(
            f"{source}={spec!r}: {module_name} has no {function_name}"
        ) from exc
    if not callable(plugin):
        raise TypeError(
            f"{source}={spec!r}: {module_name}.{function_name} is not callable"
        )
    return plugin


def register_test_path_guard(config: pytest.Config) -> None:
    """Register the selected host plugin; an empty or absent selector is a no-op."""
    spec = os.environ.get(PLUGIN_ENV)
    source = PLUGIN_ENV
    if spec is None:
        spec, source = _configured_test_plugin(config)
    plugin = resolve_test_plugin(spec, source=source)
    if plugin is not None:
        plugin(config)
