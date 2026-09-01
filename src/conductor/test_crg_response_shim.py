"""Tests for the code-review-graph response compaction shim."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from conductor import crg_response_shim as shim

REPO = Path("/repo/root")
ABS = "/repo/root/pkg/mod.py"


def _node(**overrides: Any) -> dict[str, Any]:
    node = {
        "id": 42,
        "kind": "Function",
        "name": "compact_state",
        "qualified_name": f"{ABS}::compact_state",
        "file_path": ABS,
        "line_start": 77,
        "line_end": 104,
        "language": "python",
        "parent_name": None,
        "is_test": False,
    }
    node.update(overrides)
    return node


def test_compact_relativizes_and_elides_derivable_fields() -> None:
    out = shim.compact_payload({"results": [_node()]}, REPO, keep_hints=False)
    node = out["results"][0]
    assert node == {
        "kind": "Function",
        "qualified_name": "pkg/mod.py::compact_state",
        "line_start": 77,
        "line_end": 104,
    }


def test_compact_keeps_name_when_not_derivable_and_true_is_test() -> None:
    node = _node(name="Other", is_test=True, parent_name="Cls")
    out = shim.compact_payload(node, REPO, keep_hints=False)
    assert out["name"] == "Other"
    assert out["is_test"] is True
    assert out["parent_name"] == "Cls"
    assert "file_path" not in out


def test_compact_keeps_file_path_when_no_carrier_matches() -> None:
    edge = {
        "id": 1,
        "kind": "CALLS",
        "source": "/repo/root/a.py::f",
        "target": "/repo/root/b.py::g",
        "file_path": "/repo/root/c.py",
        "line": 3,
    }
    out = shim.compact_payload(edge, REPO, keep_hints=False)
    assert out == {
        "kind": "CALLS",
        "source": "a.py::f",
        "target": "b.py::g",
        "file_path": "c.py",
        "line": 3,
    }


def test_file_node_collapses_to_qualified_name_only() -> None:
    node = _node(kind="File", name=ABS, qualified_name=ABS, line_start=1, line_end=9)
    out = shim.compact_payload(node, REPO, keep_hints=False)
    assert out == {
        "kind": "File",
        "qualified_name": "pkg/mod.py",
        "line_start": 1,
        "line_end": 9,
    }


def test_hints_dropped_by_default_and_kept_on_request(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    payload = {"status": "ok", "_hints": {"next_steps": []}}
    monkeypatch.delenv(shim.HINTS_ENV, raising=False)
    assert "_hints" not in shim.compact_payload(payload, REPO)
    monkeypatch.setenv(shim.HINTS_ENV, "1")
    assert shim.compact_payload(payload, REPO)["_hints"] == {"next_steps": []}
    assert "_hints" not in shim.compact_payload(payload, REPO, keep_hints=False)


def test_long_lists_are_cut_with_explicit_marker(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    payload = {"results": [{"kind": "Function", "n": i} for i in range(7)]}
    out = shim.compact_payload(payload, REPO, keep_hints=False, max_items=5)
    assert [r["n"] for r in out["results"][:5]] == [0, 1, 2, 3, 4]
    assert out["results"][5] == {shim.TRUNCATED_KEY: 2}
    assert len(out["results"]) == 6
    untouched = shim.compact_payload(payload, REPO, keep_hints=False, max_items=7)
    assert len(untouched["results"]) == 7
    monkeypatch.setenv(shim.MAX_LIST_ENV, "3")
    assert shim.compact_payload(payload, REPO, keep_hints=False)["results"][3] == {
        shim.TRUNCATED_KEY: 4
    }
    monkeypatch.setenv(shim.MAX_LIST_ENV, "0")
    with pytest.raises(shim.ResponseShimError, match=">= 1"):
        shim.compact_payload(payload, REPO, keep_hints=False)
    monkeypatch.delenv(shim.MAX_LIST_ENV)
    assert shim._max_list_items() == shim.DEFAULT_MAX_LIST_ITEMS


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        ("/repo/root/a.py", "a.py"),
        ("/repo/root/", ""),
        ("/repo/root", "/repo/root"),
        ("/repo/rootless/a.py", "/repo/rootless/a.py"),
        ("relative/a.py", "relative/a.py"),
        ("", ""),
    ],
)
def test_relativize_prefix_property(value: str, expected: str) -> None:
    out = shim.compact_payload(value, REPO, keep_hints=False)
    assert out == expected
    # relativizing is idempotent and never lengthens a string
    assert shim.compact_payload(out, REPO, keep_hints=False) == out
    assert len(out) <= len(value)


@pytest.mark.parametrize("max_items", [1, 2, 3, 5, 8])
def test_list_cap_property(max_items: int) -> None:
    payload = [{"kind": "F", "n": i} for i in range(6)]
    out = shim.compact_payload(payload, REPO, keep_hints=False, max_items=max_items)
    kept = [item for item in out if shim.TRUNCATED_KEY not in item]
    dropped = sum(item.get(shim.TRUNCATED_KEY, 0) for item in out)
    assert len(kept) == min(6, max_items) and len(kept) + dropped == 6


def test_non_json_payloads_pass_through() -> None:
    assert shim.compact_payload("plain text", REPO, keep_hints=False) == "plain text"
    assert shim.compact_payload(7, REPO, keep_hints=False) == 7
    assert shim.compact_payload(None, REPO, keep_hints=False) is None


def test_prefix_only_strips_repo_root_not_lookalikes() -> None:
    payload = {"a": "/repo/rootless/x.py", "b": "/repo/root/x.py"}
    out = shim.compact_payload(payload, REPO, keep_hints=False)
    assert out == {"a": "/repo/rootless/x.py", "b": "x.py"}


def _fake_mcp(tools: dict[str, Any]) -> SimpleNamespace:
    """fastmcp 3 shape: tools live in _local_provider._components as 'tool:<name>@'."""
    for name, tool in tools.items():
        tool.name = name
    components = {f"tool:{name}@": tool for name, tool in tools.items()}
    components["resource:probe@"] = SimpleNamespace(name="probe")
    return SimpleNamespace(_local_provider=SimpleNamespace(_components=components))


def test_install_wraps_sync_and_async_tools(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shim, "assert_supported_fastmcp", lambda: None)

    def sync_tool() -> dict[str, Any]:
        return {"file_path": ABS, "id": 1, "_hints": {}}

    async def async_tool() -> dict[str, Any]:
        return {"results": [_node()], "_hints": {}}

    tools = {
        "sync": SimpleNamespace(fn=sync_tool),
        "async": SimpleNamespace(fn=async_tool),
    }
    monkeypatch.delenv(shim.HINTS_ENV, raising=False)
    assert shim.install_response_shim(_fake_mcp(tools), REPO) == 2
    assert tools["sync"].fn() == {"file_path": "pkg/mod.py"}
    out = asyncio.run(tools["async"].fn())
    assert out == {
        "results": [
            {
                "kind": "Function",
                "qualified_name": "pkg/mod.py::compact_state",
                "line_start": 77,
                "line_end": 104,
            }
        ]
    }
    assert json.dumps(out)


def test_install_applies_enrichers_before_compaction(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shim, "assert_supported_fastmcp", lambda: None)
    monkeypatch.delenv(shim.HINTS_ENV, raising=False)
    tools = {
        "search": SimpleNamespace(fn=lambda: {"results": [_node()], "_hints": {}}),
        "other": SimpleNamespace(fn=lambda: {"x": ABS}),
    }

    def enrich(payload: dict[str, Any]) -> dict[str, Any]:
        for hit in payload["results"]:
            hit["doc"] = "seen " + hit["file_path"]
        return payload

    shim.install_response_shim(_fake_mcp(tools), REPO, enrichers={"search": enrich})
    hit = tools["search"].fn()["results"][0]
    assert hit["doc"] == "seen " + ABS  # enricher saw the raw absolute path
    assert hit["qualified_name"] == "pkg/mod.py::compact_state"  # then compacted
    assert tools["other"].fn() == {"x": "pkg/mod.py"}
    with pytest.raises(shim.ResponseShimError, match="unknown tools: \\['nope'\\]"):
        shim.install_response_shim(_fake_mcp(tools), REPO, enrichers={"nope": enrich})


def test_install_fails_loud_when_seam_is_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shim, "assert_supported_fastmcp", lambda: None)
    with pytest.raises(shim.ResponseShimError, match="_local_provider"):
        shim.install_response_shim(SimpleNamespace(), REPO)
    with pytest.raises(shim.ResponseShimError, match="no callable fn"):
        shim.install_response_shim(_fake_mcp({"t": SimpleNamespace(fn=None)}), REPO)


def test_registered_tools_skips_non_tool_components_and_needs_one_tool() -> None:
    tool = SimpleNamespace(fn=lambda: None)
    assert shim.registered_tools(_fake_mcp({"only": tool})) == {"only": tool}
    with pytest.raises(shim.ResponseShimError, match="holds no tools"):
        shim.registered_tools(_fake_mcp({}))


def test_hidden_tool_names_env_override(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(shim.HIDDEN_TOOLS_ENV, raising=False)
    assert shim.hidden_tool_names() == shim.DEFAULT_HIDDEN_TOOLS
    monkeypatch.setenv(shim.HIDDEN_TOOLS_ENV, " a_tool, b_tool ,")
    assert shim.hidden_tool_names() == frozenset({"a_tool", "b_tool"})
    monkeypatch.setenv(shim.HIDDEN_TOOLS_ENV, "")
    assert shim.hidden_tool_names() == frozenset()
    monkeypatch.delenv(shim.HIDDEN_TOOLS_ENV)
    monkeypatch.setenv(shim.ROLE_ENV, "static")
    static = shim.hidden_tool_names()
    assert shim.DEFAULT_HIDDEN_TOOLS < static and "get_impact_radius_tool" in static
    assert (
        "query_graph_tool" not in static and "semantic_search_nodes_tool" not in static
    )
    monkeypatch.setenv(shim.ROLE_ENV, "review")
    assert shim.hidden_tool_names() == shim.DEFAULT_HIDDEN_TOOLS | {
        "refactor_tool",
        "apply_refactor_tool",
    }
    monkeypatch.setenv(shim.ROLE_ENV, "wizard")
    with pytest.raises(shim.ResponseShimError, match="wizard"):
        shim.hidden_tool_names()
    monkeypatch.setenv(shim.HIDDEN_TOOLS_ENV, "x_tool")
    assert shim.hidden_tool_names() == frozenset({"x_tool"})  # explicit wins


def test_prune_tools_removes_only_listed_and_fails_on_unknown() -> None:
    tools = {
        name: SimpleNamespace(fn=lambda: None) for name in ("keep", "drop_a", "drop_b")
    }
    removed: list[str] = []
    mcp = _fake_mcp(tools)
    mcp._local_provider.remove_tool = removed.append
    assert shim.prune_tools(mcp, frozenset({"drop_b", "drop_a"})) == [
        "drop_a",
        "drop_b",
    ]
    assert removed == ["drop_a", "drop_b"]
    with pytest.raises(shim.ResponseShimError, match="unknown CRG tools: \\['nope'\\]"):
        shim.prune_tools(mcp, frozenset({"keep", "nope"}))
    with pytest.raises(shim.ResponseShimError, match="remove_tool"):
        shim.prune_tools(_fake_mcp(tools), frozenset({"keep"}))


def test_version_pin_rejects_drift(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shim.importlib.metadata, "version", lambda _name: "9.9.9")
    with pytest.raises(shim.ResponseShimError, match="9.9.9"):
        shim.assert_supported_fastmcp()
    monkeypatch.setattr(
        shim.importlib.metadata, "version", lambda _name: shim.EXPECTED_FASTMCP_VERSION
    )
    shim.assert_supported_fastmcp()
