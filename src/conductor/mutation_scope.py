"""Manifest validation and complete per-file test-scope inventory.

Complete scopes prove the current test inventory, while ranked tests identify
the subset whose mutation and value attribution the campaign claims. Release
admission independently discovers post-anchor tests and requires receipt value
evidence for them; this generic loader deliberately contains no release anchor.
"""

from __future__ import annotations

import ast
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Mapping, Protocol, Sequence


class CampaignError(RuntimeError):
    """A campaign is unsafe, incomplete, drifted, or could not be executed."""


@dataclass(frozen=True, slots=True)
class TestFileScope:
    """Proof of which tests a campaign covers in one file.

    A reviewed-patch campaign proves it by enumerating nodeids taken from an
    `inventory` of the file. A generated campaign proves it differently: its run
    command names the file, so every test in it runs and there is nothing to
    enumerate. `selection` says which proof this row carries.
    """

    path: str
    mode: str
    inventory: str
    nodeids: tuple[str, ...]
    selection: str = "nodeids"


class _RankedNode(Protocol):
    nodeid: str


class _CampaignWithScopes(Protocol):
    test_scopes: Mapping[str, TestFileScope]


def _require_mapping(value: object, label: str) -> Mapping[str, Any]:
    if not isinstance(value, dict):
        raise CampaignError(f"{label} must be a JSON object")
    return value


def _require_string(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise CampaignError(f"{label} must be a non-empty string")
    return value


def _require_string_list(value: object, label: str) -> tuple[str, ...]:
    if not isinstance(value, list) or not all(
        isinstance(item, str) and item for item in value
    ):
        raise CampaignError(f"{label} must be a list of non-empty strings")
    return tuple(value)


def _safe_relative_path(value: object, label: str) -> str:
    text = _require_string(value, label).replace("\\", "/")
    path = PurePosixPath(text)
    if path.is_absolute() or ".." in path.parts or text.startswith("./"):
        raise CampaignError(f"{label} must be a normalized repository-relative path")
    return path.as_posix()


def _python_test_nodeids(path: Path, relative: str) -> tuple[str, ...]:
    """Return statically declared pytest node IDs in source order."""

    try:
        tree = ast.parse(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, SyntaxError) as exc:
        raise CampaignError(
            f"cannot inventory Python tests in {relative}: {exc}"
        ) from exc
    nodeids: list[str] = []
    function_types = (ast.FunctionDef, ast.AsyncFunctionDef)
    for node in tree.body:
        if isinstance(node, function_types) and node.name.startswith("test_"):
            nodeids.append(f"{relative}::{node.name}")
            continue
        if not isinstance(node, ast.ClassDef) or not node.name.startswith("Test"):
            continue
        for child in node.body:
            if isinstance(child, function_types) and child.name.startswith("test_"):
                nodeids.append(f"{relative}::{node.name}::{child.name}")
    if not nodeids:
        raise CampaignError(f"complete Python test scope is empty: {relative}")
    return tuple(nodeids)


def _load_test_scopes(
    value: object,
    *,
    source_sha256: Mapping[str, str],
    ranked_tests: Sequence[_RankedNode],
    repo_root: Path,
) -> Mapping[str, TestFileScope]:
    """Load scopes and require every ranked test to belong to a declared scope.

    A complete Python scope must remain the exact current AST inventory in
    source order. Ranked tests may be a non-empty subset of that inventory so
    mutation/value attribution is not confused with inventory completeness.
    """

    raw_scopes = _require_mapping(value, "test_scopes")
    ranked_paths = {test.nodeid.split("::", 1)[0] for test in ranked_tests}
    scopes: dict[str, TestFileScope] = {}
    for raw_path, raw_scope in raw_scopes.items():
        path = _safe_relative_path(raw_path, "test_scopes path")
        row = _require_mapping(raw_scope, f"test_scopes[{path}]")
        mode = _require_string(row.get("mode"), f"test_scopes[{path}].mode")
        if mode not in {"complete", "partial"}:
            raise CampaignError(
                f"test_scopes[{path}].mode must be 'complete' or 'partial'"
            )
        inventory = _require_string(
            row.get("inventory"), f"test_scopes[{path}].inventory"
        )
        nodeids = _require_string_list(
            row.get("nodeids"), f"test_scopes[{path}].nodeids"
        )
        if not nodeids:
            raise CampaignError(f"test_scopes[{path}].nodeids may not be empty")
        if len(set(nodeids)) != len(nodeids):
            raise CampaignError(f"test_scopes[{path}].nodeids contains duplicates")
        wrong_file = [nodeid for nodeid in nodeids if nodeid.split("::", 1)[0] != path]
        if wrong_file:
            raise CampaignError(
                f"test_scopes[{path}] contains nodeids from another file: {wrong_file}"
            )
        if path not in source_sha256:
            raise CampaignError(f"test_scopes[{path}] is not bound in source_sha256")
        if inventory != "python_ast":
            if mode == "complete":
                raise CampaignError(
                    f"complete test scope inventory is unsupported: {inventory!r}"
                )
        elif not path.endswith(".py"):
            raise CampaignError(
                f"test_scopes[{path}].inventory='python_ast' requires a .py file"
            )
        elif mode == "complete":
            discovered = _python_test_nodeids(repo_root / path, path)
            if nodeids != discovered:
                missing = sorted(set(discovered) - set(nodeids))
                extra = sorted(set(nodeids) - set(discovered))
                raise CampaignError(
                    f"complete test scope does not match current Python inventory for "
                    f"{path}: missing={missing}, extra={extra}, "
                    f"expected_order={list(discovered)}"
                )
        if mode == "complete" and path not in ranked_paths:
            raise CampaignError(
                f"complete test_scopes[{path}] must contain at least one ranked test"
            )
        scopes[path] = TestFileScope(path, mode, inventory, nodeids)
    scoped_nodeids = {nodeid for scope in scopes.values() for nodeid in scope.nodeids}
    ranked_nodeids = {test.nodeid for test in ranked_tests}
    missing_ranked = sorted(ranked_nodeids - scoped_nodeids)
    if missing_ranked:
        raise CampaignError(
            "ranked_tests nodeids are missing from declared test_scopes: "
            f"{missing_ranked}"
        )
    return scopes


def _test_scopes_payload(
    campaign: _CampaignWithScopes,
) -> dict[str, dict[str, object]]:
    return {
        path: (
            {"mode": scope.mode, "selection": scope.selection}
            if scope.selection == "test_argv"
            else {
                "mode": scope.mode,
                "inventory": scope.inventory,
                "nodeids": list(scope.nodeids),
            }
        )
        for path, scope in sorted(campaign.test_scopes.items())
    }


def _test_scope_errors(campaign: _CampaignWithScopes, test_path: str) -> list[str]:
    scope = campaign.test_scopes.get(test_path)
    if scope is None:
        return ["campaign lacks explicit test scope"]
    if scope.mode != "complete":
        return [f"test scope is {scope.mode!r}, not 'complete'"]
    return []
