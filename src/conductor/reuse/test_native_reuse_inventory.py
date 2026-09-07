"""Focused contracts for the batched native-reuse inventory boundary."""

from __future__ import annotations

import sqlite3
from pathlib import Path

from conductor.reuse import graph_index, repo_evidence


def test_native_reuse_rows_batch_filters_and_caps_callers(tmp_path: Path) -> None:
    database = tmp_path / "graph.db"
    with sqlite3.connect(database) as connection:
        connection.execute(
            "CREATE TABLE nodes ("
            "name TEXT, file_path TEXT, line_start INTEGER, language TEXT, "
            "params TEXT, kind TEXT, is_test INTEGER, qualified_name TEXT)"
        )
        connection.execute(
            "CREATE TABLE edges (kind TEXT, target_qualified TEXT, source_qualified TEXT)"
        )
        connection.executemany(
            "INSERT INTO nodes VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    "parse_batch",
                    str(tmp_path / "pkg" / "parser.py"),
                    17,
                    "python",
                    "(rows)",
                    "Function",
                    0,
                    "pkg.parser.parse_batch",
                ),
                (
                    "test_parse_batch",
                    str(tmp_path / "tests" / "test_parser.py"),
                    9,
                    "python",
                    "()",
                    "Function",
                    1,
                    "tests.test_parser.test_parse_batch",
                ),
                (
                    "parse_batch_js",
                    str(tmp_path / "web" / "parser.js"),
                    4,
                    "javascript",
                    "(rows)",
                    "Function",
                    0,
                    "web.parser.parse_batch_js",
                ),
                (
                    "external_parse_batch",
                    "/outside/parser.py",
                    3,
                    "python",
                    "(rows)",
                    "Function",
                    0,
                    "outside.parser.parse_batch",
                ),
            ],
        )
        callers = [
            ("CALLS", "pkg.parser.parse_batch", f"consumer.{index}")
            for index in range(25)
        ]
        callers.append(("REFERENCES", "pkg.parser.parse_batch", "consumer.0"))
        connection.executemany("INSERT INTO edges VALUES (?, ?, ?)", callers)

    rows = graph_index.GraphIndex(tmp_path, database).native_reuse_rows()

    assert rows == [
        {
            "name": "parse_batch",
            "file": "pkg/parser.py",
            "line_start": 17,
            "language": "python",
            "params": "(rows)",
            "caller_count": 20,
        }
    ]


def test_native_reuse_normalizes_suffix_noise_and_uses_callers() -> None:
    python = graph_index.Symbol(
        "Function",
        "interaction_metrics",
        "python::interaction_metrics",
        "research/eval/metrics.py",
        10,
        20,
        "python",
        "(x)",
        False,
    )
    native = graph_index.Symbol(
        "Function",
        "interaction_metrics_f32_py",
        "cpp::interaction_metrics_f32_py",
        "aria_core/bindings/bind_graph.cpp",
        440,
        453,
        "cpp",
        "(x)",
        False,
    )

    class FakeIndex:
        def symbols(self, **kwargs):
            return [python, native]

        def callers(self, qualified_name: str):
            return ["consumer"]

    candidates = repo_evidence.native_reuse_candidates(FakeIndex())  # type: ignore[arg-type]

    assert len(candidates) == 1
    assert candidates[0]["native_targets"] == [native.stable_id]
    assert candidates[0]["severity"] == "high"
    assert candidates[0]["evidence_complete"] is True


def test_native_reuse_filters_common_symbol_names() -> None:
    common_python = graph_index.Symbol(
        "Function",
        "forward",
        "python::forward",
        "runner.py",
        1,
        2,
        "python",
        "()",
        False,
    )
    common_native = graph_index.Symbol(
        "Function",
        "forward_native",
        "rust::forward_native",
        "runner.rs",
        1,
        2,
        "rust",
        "()",
        False,
    )

    class FakeIndex:
        def symbols(self, **kwargs):
            return [common_python, common_native]

        def callers(self, qualified_name: str):
            return ["consumer"]

    assert repo_evidence.native_reuse_candidates(FakeIndex()) == []  # type: ignore[arg-type]
