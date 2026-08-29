#!/usr/bin/env python3
"""Token-compaction shim for code-review-graph MCP responses.

Installed by ``conductor.crg_server`` before the FastMCP server starts. Every
registered tool's return payload is rewritten in place:

* repository-absolute paths become repo-relative,
* ``None`` values, internal ``id`` fields, and fields derivable from a sibling
  (``file_path`` when ``qualified_name``/``source`` already carries it, ``name``
  when ``qualified_name`` ends with it, ``language`` from the extension,
  ``is_test`` when false) are dropped,
* the ``_hints`` block is removed unless ``CRG_KEEP_HINTS=1``,
* lists longer than ``CRG_MAX_LIST_ITEMS`` (default 150) are cut and end with
  an explicit ``{"_truncated": <dropped count>}`` marker. Unbounded lists are
  how ``get_impact_radius`` on one hub file returned 437 KB (~110k tokens).

Every surviving value is the original value or its repo-relative form; the
only lossy step is the marked truncation. Measured over six live calls:
446,112 -> 225,422 bytes before the list cap (49 % saved).
"""

from __future__ import annotations

import importlib.metadata
import inspect
import os
from collections.abc import Callable
from functools import wraps
from pathlib import Path
from typing import Any, Final

EXPECTED_FASTMCP_VERSION: Final[str] = "2.14.6"
HINTS_ENV: Final[str] = "CRG_KEEP_HINTS"
MAX_LIST_ENV: Final[str] = "CRG_MAX_LIST_ITEMS"
DEFAULT_MAX_LIST_ITEMS: Final[int] = 150
TRUNCATED_KEY: Final[str] = "_truncated"
DROP_KEYS: Final[frozenset[str]] = frozenset({"id", "language"})
PATH_CARRIERS: Final[tuple[str, ...]] = ("qualified_name", "source", "target")
HIDDEN_TOOLS_ENV: Final[str] = "CRG_HIDDEN_TOOLS"
# Never used by agents here (hooks own builds/embeds; wiki/registry are unused);
# their schemas cost ~9 KB per session on harnesses that load every tool.
DEFAULT_HIDDEN_TOOLS: Final[frozenset[str]] = frozenset(
    {
        "build_or_update_graph_tool",
        "embed_graph_tool",
        "run_postprocess_tool",
        "generate_wiki_tool",
        "get_wiki_page_tool",
        "get_docs_section_tool",
        "list_repos_tool",
        "cross_repo_search_tool",
    }
)


class ResponseShimError(RuntimeError):
    """The FastMCP seam the shim relies on is unavailable."""


def _relativize(text: str, prefix: str) -> str:
    if text.startswith(prefix):
        return text[len(prefix) :]
    return text


def _elide_derivable(node: dict[str, Any]) -> None:
    file_path = node.get("file_path")
    if isinstance(file_path, str):
        for key in PATH_CARRIERS:
            carrier = node.get(key)
            if isinstance(carrier, str) and (
                carrier == file_path or carrier.startswith(file_path + "::")
            ):
                del node["file_path"]
                break
    name = node.get("name")
    qualified = node.get("qualified_name")
    if isinstance(name, str) and isinstance(qualified, str):
        if qualified == name or qualified.endswith("::" + name):
            del node["name"]
    if node.get("is_test") is False:
        del node["is_test"]


def compact_value(value: Any, prefix: str, *, keep_hints: bool, max_items: int) -> Any:
    """Recursively compact one JSON-like value."""
    if isinstance(value, str):
        return _relativize(value, prefix)
    if isinstance(value, list):
        kept = [
            compact_value(item, prefix, keep_hints=keep_hints, max_items=max_items)
            for item in value[:max_items]
        ]
        dropped = len(value) - len(kept)
        if dropped > 0:
            kept.append({TRUNCATED_KEY: dropped})
        return kept
    if isinstance(value, dict):
        out: dict[str, Any] = {}
        for key, item in value.items():
            if item is None or key in DROP_KEYS:
                continue
            if key == "_hints" and not keep_hints:
                continue
            out[key] = compact_value(
                item, prefix, keep_hints=keep_hints, max_items=max_items
            )
        _elide_derivable(out)
        return out
    return value


def _max_list_items() -> int:
    raw = os.environ.get(MAX_LIST_ENV, "").strip()
    if not raw:
        return DEFAULT_MAX_LIST_ITEMS
    value = int(raw)
    if value < 1:
        raise ResponseShimError(f"{MAX_LIST_ENV} must be >= 1, got {value}")
    return value


def compact_payload(
    payload: Any,
    repo_root: Path,
    *,
    keep_hints: bool | None = None,
    max_items: int | None = None,
) -> Any:
    """Compact a tool payload; non-JSON payloads pass through untouched."""
    if keep_hints is None:
        keep_hints = os.environ.get(HINTS_ENV, "").strip() == "1"
    if max_items is None:
        max_items = _max_list_items()
    prefix = str(Path(repo_root).resolve()) + "/"
    return compact_value(payload, prefix, keep_hints=keep_hints, max_items=max_items)


Enricher = Callable[[Any], Any]


def _wrap(
    fn: Callable[..., Any], repo_root: Path, enrich: Enricher | None = None
) -> Callable[..., Any]:
    def finish(result: Any) -> Any:
        if enrich is not None:
            result = enrich(result)
        return compact_payload(result, repo_root)

    if inspect.iscoroutinefunction(fn):

        @wraps(fn)
        async def async_wrapper(*args: Any, **kwargs: Any) -> Any:
            return finish(await fn(*args, **kwargs))

        return async_wrapper

    @wraps(fn)
    def wrapper(*args: Any, **kwargs: Any) -> Any:
        return finish(fn(*args, **kwargs))

    return wrapper


def assert_supported_fastmcp() -> None:
    version = importlib.metadata.version("fastmcp")
    if version != EXPECTED_FASTMCP_VERSION:
        raise ResponseShimError(
            f"fastmcp version {version!r} != {EXPECTED_FASTMCP_VERSION!r}; "
            "re-verify the _tool_manager/_tools/fn seam before bumping"
        )


def registered_tools(mcp: Any) -> dict[str, Any]:
    """Return FastMCP's registered tool objects, failing loud if the seam moved."""
    manager = getattr(mcp, "_tool_manager", None)
    tools = getattr(manager, "_tools", None)
    if not isinstance(tools, dict) or not tools:
        raise ResponseShimError(
            "FastMCP tool registry (_tool_manager._tools) not found"
        )
    for name, tool in tools.items():
        if not callable(getattr(tool, "fn", None)):
            raise ResponseShimError(f"tool {name!r} has no callable fn attribute")
    return tools


ROLE_ENV: Final[str] = "CRG_ROLE"
# Role surfaces: launch an agent with CRG_ROLE=static and its server exposes only
# the orientation/lookup tools it actually uses (schema bytes are per session).
ROLE_HIDDEN_TOOLS: Final[dict[str, frozenset[str]]] = {
    "full": DEFAULT_HIDDEN_TOOLS,
    "review": DEFAULT_HIDDEN_TOOLS | {"refactor_tool", "apply_refactor_tool"},
    "static": DEFAULT_HIDDEN_TOOLS
    | {
        "detect_changes_tool",
        "get_review_context_tool",
        "get_impact_radius_tool",
        "get_affected_flows_tool",
        "list_flows_tool",
        "get_flow_tool",
        "list_communities_tool",
        "get_community_tool",
        "get_architecture_overview_tool",
        "refactor_tool",
        "apply_refactor_tool",
    },
}


def hidden_tool_names() -> frozenset[str]:
    """Tools to unregister: explicit ``CRG_HIDDEN_TOOLS`` wins, else the ``CRG_ROLE`` set."""
    raw = os.environ.get(HIDDEN_TOOLS_ENV)
    if raw is not None:
        return frozenset(name.strip() for name in raw.split(",") if name.strip())
    role = os.environ.get(ROLE_ENV, "").strip() or "full"
    if role not in ROLE_HIDDEN_TOOLS:
        raise ResponseShimError(
            f"{ROLE_ENV}={role!r} unknown; choose one of {sorted(ROLE_HIDDEN_TOOLS)}"
        )
    return ROLE_HIDDEN_TOOLS[role]


def prune_tools(mcp: Any, hidden: frozenset[str] | None = None) -> list[str]:
    """Unregister *hidden* tools; unknown names fail loud. Returns removed names."""
    names = hidden_tool_names() if hidden is None else hidden
    tools = registered_tools(mcp)
    unknown = sorted(names - tools.keys())
    if unknown:
        raise ResponseShimError(f"cannot hide unknown CRG tools: {unknown}")
    if not callable(getattr(mcp, "remove_tool", None)):
        raise ResponseShimError("FastMCP server has no remove_tool()")
    for name in sorted(names):
        mcp.remove_tool(name)
    return sorted(names)


def install_response_shim(
    mcp: Any, repo_root: Path, enrichers: dict[str, Enricher] | None = None
) -> int:
    """Wrap every registered tool fn; returns the number of tools wrapped.

    *enrichers* maps a tool name to a function applied to its raw result
    before compaction (e.g. adding docstrings to search hits). Unknown names
    fail loud so a renamed tool cannot silently lose its enrichment.
    """
    assert_supported_fastmcp()
    tools = registered_tools(mcp)
    enrichers = enrichers or {}
    unknown = sorted(enrichers.keys() - tools.keys())
    if unknown:
        raise ResponseShimError(f"enrichers reference unknown tools: {unknown}")
    for name, tool in tools.items():
        tool.fn = _wrap(tool.fn, repo_root, enrichers.get(name))
    return len(tools)
