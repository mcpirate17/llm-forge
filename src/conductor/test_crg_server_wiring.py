"""Tests for the workspace wiring in ``conductor.crg_server``."""

from __future__ import annotations

import sys
import types
from pathlib import Path
from typing import Any

import pytest

from conductor import crg_server


@pytest.fixture
def wired(monkeypatch: pytest.MonkeyPatch) -> list[tuple[str, Any]]:
    calls: list[tuple[str, Any]] = []
    fake_mcp = object()
    fake_main = types.ModuleType("code_review_graph.main")
    fake_main.mcp = fake_mcp  # type: ignore[attr-defined]
    fake_main.main = lambda repo_root=None: calls.append(("crg_main", repo_root))  # type: ignore[attr-defined]
    monkeypatch.setitem(
        sys.modules, "code_review_graph", types.ModuleType("code_review_graph")
    )
    monkeypatch.setitem(sys.modules, "code_review_graph.main", fake_main)
    monkeypatch.setattr(
        crg_server, "install_bridge", lambda: calls.append(("bridge", None))
    )
    monkeypatch.setattr(
        crg_server, "install_node_text", lambda root: calls.append(("node_text", root))
    )
    monkeypatch.setattr(
        crg_server,
        "register_workspace_tools",
        lambda mcp: calls.append(("register", mcp is fake_mcp)),
    )
    monkeypatch.setattr(
        crg_server, "prune_tools", lambda mcp: calls.append(("prune", mcp is fake_mcp))
    )
    monkeypatch.setattr(crg_server, "search_enrichers", lambda root: {"enrich": root})
    monkeypatch.setattr(
        crg_server,
        "install_response_shim",
        lambda mcp, root, enrichers: calls.append(
            ("shim", (mcp is fake_mcp, root, enrichers))
        ),
    )
    return calls


def test_main_wires_everything_in_order(
    wired: list[tuple[str, Any]], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("CRG_SHIM_DISABLE", raising=False)
    assert crg_server.main(["--repo", "/r"]) == 0
    assert wired == [
        ("bridge", None),
        ("node_text", Path("/r")),
        ("register", True),
        ("prune", True),
        ("shim", (True, Path("/r"), {"enrich": Path("/r")})),
        ("crg_main", "/r"),
    ]


def test_shim_disable_skips_workspace_wiring_but_keeps_node_text(
    wired: list[tuple[str, Any]], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("CRG_SHIM_DISABLE", "1")
    assert crg_server.main([]) == 0
    assert [name for name, _ in wired] == ["bridge", "node_text", "crg_main"]
    assert wired[1][1] == crg_server.ROOT


def test_bridge_error_invariant_fails_loud(
    wired: list[tuple[str, Any]], monkeypatch: pytest.MonkeyPatch
) -> None:
    from conductor.crg_embedding_bridge import CrgBridgeError

    def boom() -> None:
        raise CrgBridgeError("version drift")

    monkeypatch.setattr(crg_server, "install_bridge", boom)
    with pytest.raises(SystemExit):
        crg_server.main([])
    assert wired == []


def test_shim_errors_fail_loud(
    wired: list[tuple[str, Any]], monkeypatch: pytest.MonkeyPatch
) -> None:
    from conductor.crg_response_shim import ResponseShimError

    def boom(mcp: Any) -> None:
        raise ResponseShimError("seam moved")

    monkeypatch.delenv("CRG_SHIM_DISABLE", raising=False)
    monkeypatch.setattr(crg_server, "prune_tools", boom)
    with pytest.raises(SystemExit):
        crg_server.main([])
    assert ("crg_main", None) not in wired
