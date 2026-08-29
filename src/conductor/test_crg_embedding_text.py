"""Tests for path+docstring embedding text."""

from __future__ import annotations

import sys
import types
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from conductor import crg_embedding_text as cet

SOURCE = '''\
"""Module doc."""


def plain(a: int) -> int:
    """First line of plain.

    Second paragraph.
    """
    return a


@decorator
def decorated():
    """Decorated doc."""


class Widget:
    """Widget class doc."""

    def method(self, x):
        """Method doc."""

    def nodoc(self):
        pass


def plain_twin():
    """Twin."""
'''


@pytest.fixture
def module(tmp_path: Path) -> Path:
    path = tmp_path / "pkg" / "mod.py"
    path.parent.mkdir()
    path.write_text(SOURCE, encoding="utf-8")
    cet._python_docstrings.cache_clear()
    return path


def _node(path: Path, name: str, line: int, **extra: Any) -> SimpleNamespace:
    base: dict[str, Any] = {
        "kind": "Function",
        "name": name,
        "qualified_name": f"{path}::{name}",
        "file_path": str(path),
        "line_start": line,
        "parent_name": None,
        "params": None,
        "return_type": None,
    }
    base.update(extra)
    return SimpleNamespace(**base)


def test_docstrings_for_functions_methods_classes_and_decorators(module: Path) -> None:
    assert cet.docstring_for(_node(module, "plain", 4)) == "First line of plain."
    # tree-sitter starts decorated nodes on the decorator line; ast on the def.
    assert cet.docstring_for(_node(module, "decorated", 13)) == "Decorated doc."
    assert (
        cet.docstring_for(_node(module, "Widget", 18, kind="Class"))
        == "Widget class doc."
    )
    assert (
        cet.docstring_for(
            _node(
                module,
                "method",
                21,
                parent_name="Widget",
                qualified_name=f"{module}::Widget.method",
            )
        )
        == "Method doc."
    )
    assert cet.docstring_for(_node(module, "nodoc", 24, parent_name="Widget")) == ""


def test_docstring_prefers_line_proximity_then_unique_name(module: Path) -> None:
    # Wrong line but unique name -> still resolved.
    assert cet.docstring_for(_node(module, "plain_twin", 999)) == "Twin."
    assert cet.docstring_for(_node(module, "missing", 1)) == ""


def test_non_python_and_unreadable_files_yield_no_docstring(tmp_path: Path) -> None:
    rust = tmp_path / "lib.rs"
    rust.write_text("fn main() {}", encoding="utf-8")
    assert cet.docstring_for(_node(rust, "main", 1)) == ""
    assert cet.docstring_for(_node(tmp_path / "gone.py", "x", 1)) == ""
    broken = tmp_path / "broken.py"
    broken.write_text("def (:\n", encoding="utf-8")
    assert cet.docstring_for(_node(broken, "x", 1)) == ""


def test_node_text_is_relative_and_bounded(module: Path, tmp_path: Path) -> None:
    node = _node(
        module,
        "plain",
        4,
        params="(a: int)",
        return_type="int",
    )
    assert cet.node_text(node, repo_root=tmp_path) == (
        "pkg/mod.py::plain function (a: int) returns int — First line of plain."
    )
    node.params = "(\n    a: int,\n    *,\n    b: str,\n)"
    assert "( a: int, *, b: str, )" in cet.node_text(node, repo_root=tmp_path)
    node.params = "(" + "x, " * 400 + ")"
    text = cet.node_text(node, repo_root=tmp_path)
    assert len(text) == cet.TEXT_CHARS and text.endswith("…")


def test_docstring_cache_is_keyed_by_mtime_and_size(module: Path) -> None:
    node = _node(module, "plain", 4)
    assert cet.docstring_for(node) == "First line of plain."
    module.write_text(
        SOURCE.replace("First line of plain.", "Changed."), encoding="utf-8"
    )
    assert cet.docstring_for(node) == "Changed."
    assert cet._python_docstrings.cache_info().maxsize == 64


def test_install_replaces_pinned_node_to_text(
    monkeypatch: pytest.MonkeyPatch, module: Path, tmp_path: Path
) -> None:
    fake_embeddings = types.ModuleType("code_review_graph.embeddings")
    fake_embeddings._node_to_text = lambda node: "stock"  # type: ignore[attr-defined]
    fake_pkg = types.ModuleType("code_review_graph")
    monkeypatch.setitem(sys.modules, "code_review_graph", fake_pkg)
    monkeypatch.setitem(sys.modules, "code_review_graph.embeddings", fake_embeddings)
    calls: list[str] = []
    fake_bridge = types.ModuleType("conductor.crg_embedding_bridge")
    fake_bridge.assert_supported_crg = lambda: calls.append("checked")  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "conductor.crg_embedding_bridge", fake_bridge)

    cet.install_node_text(repo_root=tmp_path)
    assert calls == ["checked"]
    assert fake_embeddings._node_to_text(_node(module, "plain", 4)).startswith(  # type: ignore[attr-defined]
        "pkg/mod.py::plain function"
    )


def test_first_line_boundary_returns_empty_for_whitespace_only_docstring() -> None:
    assert cet._first_line(None) == ""
    assert cet._first_line("") == ""
    assert cet._first_line(" \n \n ") == ""
