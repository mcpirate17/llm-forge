from __future__ import annotations

import ast
import hashlib
import json
from collections import defaultdict
from typing import Any

import pytest

from conductor._native import duplicate_body_fingerprints_native


def _digest(value: ast.AST) -> str:
    payload = ast.dump(value, include_attributes=False)
    return hashlib.sha256(payload.encode()).hexdigest()


def _candidate_digest(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
) -> str | None:
    body = list(node.body)
    if (
        body
        and isinstance(body[0], ast.Expr)
        and isinstance(body[0].value, ast.Constant)
        and isinstance(body[0].value.value, str)
    ):
        body = body[1:]
    length = (node.end_lineno or node.lineno) - node.lineno + 1
    if length < 10 or not body:
        return None
    return _digest(ast.Module(body=body, type_ignores=[]))


def _standalone_digest(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
) -> str | None:
    end_lineno = node.end_lineno or node.lineno
    if end_lineno - node.lineno + 1 < 8:
        return None
    clone = ast.FunctionDef(
        name="_",
        args=node.args,
        body=node.body,
        decorator_list=[],
        returns=node.returns,
        type_comment=getattr(node, "type_comment", None),
    )
    ast.fix_missing_locations(clone)
    return _digest(clone)


def python_fingerprints(
    records: list[tuple[str, str]], policy: str
) -> list[dict[str, Any]]:
    digest_function = _candidate_digest if policy == "candidate" else _standalone_digest
    files: list[dict[str, Any]] = []
    for path, source in records:
        try:
            tree = ast.parse(source, filename=path)
        except (SyntaxError, ValueError, RecursionError):
            files.append({"path": path, "parse_error": True, "functions": []})
            continue
        functions: list[dict[str, Any]] = []
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            digest = digest_function(node)
            if digest is None:
                continue
            functions.append(
                {
                    "name": node.name,
                    "lineno": node.lineno,
                    "end_lineno": node.end_lineno or node.lineno,
                    "digest": digest,
                }
            )
        files.append({"path": path, "parse_error": False, "functions": functions})
    return files


def _groups(functions: list[dict[str, object]]) -> list[list[tuple[object, ...]]]:
    by_digest: dict[str, list[tuple[object, ...]]] = defaultdict(list)
    for function in functions:
        by_digest[str(function["digest"])].append(
            (function["name"], function["lineno"], function["end_lineno"])
        )
    return sorted(sorted(group) for group in by_digest.values())


def _long_function(
    name: str, argument: str, docstring: str, *, async_: bool = False
) -> str:
    prefix = "async " if async_ else ""
    return f'''{prefix}def {name}({argument}) -> int:
    "{docstring}"
    total = 1
    total += 2
    total += 3
    total += 4
    total += 5
    total += 6
    total += 7
    return total
'''


def test_native_fingerprints_preserve_cpython_equivalence_partitions() -> None:
    source = "\n".join(
        [
            _long_function("alpha", "left", "first"),
            _long_function("beta", "right, extra=1", "second"),
            _long_function("gamma", "left", "first", async_=True),
            _long_function("delta", "left", "second"),
        ]
    )

    for policy in ("candidate", "standalone"):
        native = json.loads(
            duplicate_body_fingerprints_native([("sample.py", source)], policy)
        )[0]
        reference = python_fingerprints([("sample.py", source)], policy)[0]["functions"]

        assert native["parse_error"] is False
        assert [
            (item["name"], item["lineno"], item["end_lineno"])
            for item in native["functions"]
        ] == [(item["name"], item["lineno"], item["end_lineno"]) for item in reference]
        assert _groups(native["functions"]) == _groups(reference)
    _assert_policy_distinctions()
    _assert_thresholds_nesting_and_parse_failures()
    _assert_unknown_policy_fails()


def _assert_policy_distinctions() -> None:
    source = "\n".join(
        [
            _long_function("alpha", "left", "first"),
            _long_function("beta", "right, extra=1", "second"),
            _long_function("gamma", "left", "first", async_=True),
            _long_function("delta", "left", "second"),
        ]
    )

    candidate, standalone = [
        json.loads(duplicate_body_fingerprints_native([("sample.py", source)], policy))[
            0
        ]["functions"]
        for policy in ("candidate", "standalone")
    ]

    assert len({item["digest"] for item in candidate}) == 1
    assert standalone[0]["digest"] == standalone[2]["digest"]
    assert standalone[0]["digest"] != standalone[1]["digest"]
    assert standalone[0]["digest"] != standalone[3]["digest"]


def test_candidate_strips_docstrings_but_standalone_preserves_signature_and_docstring() -> (
    None
):
    _assert_policy_distinctions()


def _assert_thresholds_nesting_and_parse_failures() -> None:
    source = """def outer():
    value = 1
    def nested():
        value = 1
        value += 2
        value += 3
        value += 4
        value += 5
        value += 6
        value += 7
        value += 8
        return value
    value += 2
    value += 3
    value += 4
    value += 5
    value += 6
    return value

def eight_lines():
    value = 1
    value += 2
    value += 3
    value += 4
    value += 5
    value += 6
    return value
"""
    records = [("nested.py", source), ("bad.py", "def broken(:\n")]

    candidate = json.loads(duplicate_body_fingerprints_native(records, "candidate"))
    standalone = json.loads(duplicate_body_fingerprints_native(records, "standalone"))

    assert [item["name"] for item in candidate[0]["functions"]] == ["outer", "nested"]
    assert [item["name"] for item in standalone[0]["functions"]] == [
        "outer",
        "eight_lines",
        "nested",
    ]
    assert candidate[1] == {"path": "bad.py", "parse_error": True, "functions": []}
    assert standalone[1] == candidate[1]


def test_thresholds_nested_order_and_parse_failures_are_exact() -> None:
    _assert_thresholds_nesting_and_parse_failures()


def _assert_unknown_policy_fails() -> None:
    with pytest.raises(ValueError, match="unknown duplicate-body policy"):
        duplicate_body_fingerprints_native([], "approximate")


def test_unknown_policy_fails_loudly() -> None:
    _assert_unknown_policy_fails()
