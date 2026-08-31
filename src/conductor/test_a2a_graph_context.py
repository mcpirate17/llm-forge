"""Production contracts for bounded A2A AST and code-graph context."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from conductor import a2a_cli
from conductor import a2a_graph_context as graph_context
from conductor.agent_a2a import A2aStore
from conductor.graph_context import FileContextSummary, GraphRelationship


def _write_module(repo: Path, relative_path: str, source: str) -> Path:
    path = repo / relative_path
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(source, encoding="utf-8")
    return path


def test_extract_context_refs_is_repo_bounded_deduplicated_and_limited(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "repo"
    first = _write_module(repo, "pkg/a.py", "def first() -> None: ...\n")
    _write_module(repo, "pkg/b.py", "def second() -> None: ...\n")
    _write_module(repo, "pkg/c.py", "def third() -> None: ...\n")
    outside = _write_module(tmp_path, "outside.py", "def escape() -> None: ...\n")
    fragments = [
        {
            "message_id": "m-1",
            "body": (
                f"reject {outside}::escape then inspect pkg/a.py::first "
                "and pkg/b.py::second"
            ),
            "data_json": "",
        },
        {
            "message_id": "m-2",
            "body": (f"repeat {first}::first then pkg/c.py::third"),
            "data_json": "",
        },
    ]

    refs = graph_context.extract_context_refs(repo, fragments, max_refs=2)

    assert [(ref.path, ref.symbol) for ref in refs] == [
        ("pkg/a.py", "first"),
        ("pkg/b.py", "second"),
    ]
    assert refs[0].message_ids == ("m-1", "m-2")


def test_bounded_context_requests_graph_and_includes_ast_relationships(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = tmp_path / "repo"
    _write_module(repo, "pkg/mod.py", "def ping() -> str:\n    return 'pong'\n")
    calls: list[tuple[Path, str, str | None, bool]] = []

    def fake_get_file_context(
        root: Path,
        file_path: str,
        *,
        target_symbol: str | None,
        with_graph: bool,
    ) -> FileContextSummary:
        calls.append((root, file_path, target_symbol, with_graph))
        return FileContextSummary(
            file_path=file_path,
            skeleton="def ping() -> str: ...",
            symbols=["ping"],
            callers=[GraphRelationship("pkg.caller", "CALLS", "pkg/caller.py")],
            callees=[GraphRelationship("pkg.callee", "CALLS", "pkg/callee.py")],
            graph_status="ok",
        )

    monkeypatch.setattr(graph_context, "get_file_context", fake_get_file_context)
    result = graph_context.build_bounded_code_context(
        repo,
        [{"message_id": "m-graph", "body": "pkg/mod.py::ping", "data_json": ""}],
        max_chars=600,
    )

    assert calls == [(repo, "pkg/mod.py", "ping", True)]
    assert result["contexts"] == [
        {
            "messages": ["m-graph"],
            "path": "pkg/mod.py",
            "symbol": "ping",
            "ast": "def ping() -> str: ...",
            "callers": ["pkg.caller"],
            "callees": ["pkg.callee"],
            "graph_status": "ok",
        }
    ]


def test_bounded_context_enforces_serialized_budget_and_omits_raw_body(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = tmp_path / "repo"
    _write_module(repo, "pkg/mod.py", "def ping() -> None: ...\n")
    raw_marker = "raw-message-marker-must-not-enter-context"

    def huge_context(*_args: Any, **_kwargs: Any) -> FileContextSummary:
        relationships = [
            GraphRelationship(f"pkg.{'caller' * 40}.{index}", "CALLS", "pkg/x.py")
            for index in range(8)
        ]
        return FileContextSummary(
            file_path="pkg/mod.py",
            skeleton="def ping(value: str) -> str: " + "detail " * 300,
            symbols=["ping"],
            callers=relationships,
            callees=relationships,
            graph_status="ok",
        )

    monkeypatch.setattr(graph_context, "get_file_context", huge_context)
    result = graph_context.build_bounded_code_context(
        repo,
        [
            {
                "message_id": "m-budget",
                "body": f"{raw_marker} inspect pkg/mod.py::ping",
                "data_json": "",
            }
        ],
        max_chars=320,
    )
    serialized = json.dumps(result, sort_keys=True, separators=(",", ":"))

    assert len(serialized) <= 320
    assert raw_marker not in serialized


def test_store_context_fragments_are_scan_bounded(tmp_path: Path) -> None:
    store = A2aStore(tmp_path, "tester-a")
    body = "b" * 512 + " pkg/late.py::missed"
    data_json = "d" * 512 + " pkg/also_late.py::missed"
    store.record_inbound("m-scan", "tester-b", "tester-a", body, data_json)

    fragments = graph_context.read_context_fragments(
        store.path, ["m-scan"], scan_chars=256
    )

    assert fragments == [
        {
            "message_id": "m-scan",
            "body": "b" * 256,
            "data_json": "d" * 256,
        }
    ]


def test_watch_once_attaches_code_context_and_marks_presentation(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repo = tmp_path / "repo"
    _write_module(repo, "pkg/mod.py", "def ping() -> str:\n    return 'pong'\n")
    state_dir = tmp_path / "state"
    store = A2aStore(state_dir, "tester-a")
    store.record_inbound(
        "m-watch",
        "tester-b",
        "tester-a",
        "Please inspect pkg/mod.py::ping",
        None,
    )
    command = [
        "--state-dir",
        str(state_dir),
        "watch",
        "--as-name",
        "tester-a",
        "--once",
        "--json",
        "--repo-root",
        str(repo),
    ]

    assert a2a_cli.main(command) == 0
    payload = json.loads(capsys.readouterr().out)
    context = payload["code_context"]
    assert context["authority"] == graph_context.AUTHORITY
    assert context["contexts"][0]["path"] == "pkg/mod.py"
    assert "def ping() -> str:" in context["contexts"][0]["ast"]

    assert a2a_cli.main(command) == 0
    assert capsys.readouterr().out == ""


def test_no_reference_avoids_graph_lookup(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    def unexpected_lookup(*_args: Any, **_kwargs: Any) -> FileContextSummary:
        raise AssertionError("graph lookup must not run without a concrete reference")

    monkeypatch.setattr(graph_context, "get_file_context", unexpected_lookup)
    result = graph_context.build_bounded_code_context(
        tmp_path,
        [{"message_id": "m-none", "body": "status is green", "data_json": "{}"}],
    )

    assert result["contexts"] == []
    assert result["omitted_refs"] == 0
