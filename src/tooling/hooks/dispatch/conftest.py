"""Skip the one dispatch test whose subject only exists inside a host project.

`conductor init` writes the dispatch launcher into the project it is initialising;
the package does not ship one, so an uninitialised standalone install has nothing
for `test_launcher_is_tracked_and_executable` to check. `conductor/_project_hooks.py`
documents CONDUCTOR_PROJECT_TEST_PLUGIN="" as exactly that condition.

This lives beside the test rather than as a decorator on it for the same reason as
`conductor/conftest.py`'s inventory: the test states what it proves, and where it
cannot run is recorded separately, as debt.
"""

from __future__ import annotations

import os

import pytest

HOST_PROJECT_TESTS = {
    "test_launcher_is_tracked_and_executable": (
        "no host project: the launcher is written by `conductor init`, not shipped"
    ),
}


def pytest_collection_modifyitems(
    config: pytest.Config, items: list[pytest.Item]
) -> None:
    if os.environ.get("CONDUCTOR_PROJECT_TEST_PLUGIN") != "":
        return
    for item in items:
        reason = HOST_PROJECT_TESTS.get(item.originalname or item.name)
        if reason is not None:
            item.add_marker(pytest.mark.skip(reason=reason))
