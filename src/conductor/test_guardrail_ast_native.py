from __future__ import annotations

import ast
import json
from pathlib import Path

import pytest

from conductor import guardrail_audit
from conductor._native import guardrail_ast_metrics_native


def _max_nesting(node: ast.AST) -> int:
    control = (
        ast.If,
        ast.For,
        ast.AsyncFor,
        ast.While,
        ast.Try,
        ast.With,
        ast.AsyncWith,
        ast.Match,
    )

    def walk(child: ast.AST, depth: int) -> int:
        best = depth
        for descendant in ast.iter_child_nodes(child):
            next_depth = depth + 1 if isinstance(descendant, control) else depth
            best = max(best, walk(descendant, next_depth))
        return best

    return walk(node, 0)


def _hot_loop(node: ast.AST) -> bool:
    for child in ast.walk(node):
        if not isinstance(child, (ast.For, ast.AsyncFor)):
            continue
        calls = [item for item in ast.walk(child) if isinstance(item, ast.Call)]
        has_append = any(
            isinstance(call.func, ast.Attribute) and call.func.attr == "append"
            for call in calls
        )
        has_numeric = any(
            isinstance(item, (ast.BinOp, ast.AugAssign)) for item in ast.walk(child)
        )
        if has_append and has_numeric:
            return True
        if (
            getattr(child.iter, "id", "")
            in {
                "x",
                "xs",
                "arr",
                "array",
                "tensor",
                "values",
            }
            and has_numeric
        ):
            return True
    return False


def _reference_metrics(path: str, source: str) -> dict[str, object]:
    functions: list[dict[str, object]] = []

    class Collector(ast.NodeVisitor):
        def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
            self._collect(node)

        def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
            self._collect(node)

        def _collect(self, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
            functions.append(
                {
                    "symbol": node.name,
                    "lineno": node.lineno,
                    "end_lineno": node.end_lineno,
                    "branches": sum(
                        isinstance(
                            child,
                            (
                                ast.If,
                                ast.For,
                                ast.AsyncFor,
                                ast.While,
                                ast.Try,
                                ast.Match,
                                ast.IfExp,
                            ),
                        )
                        for child in ast.walk(node)
                    ),
                    "max_nesting": _max_nesting(node),
                    "is_route_registration": node.name.startswith("register_")
                    and any(
                        isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef))
                        for child in ast.iter_child_nodes(node)
                    ),
                    "hot_loop": _hot_loop(node),
                }
            )
            self.generic_visit(node)

    Collector().visit(ast.parse(source, filename=path))
    return {"path": path, "parse_error": False, "functions": functions}


def test_native_metrics_match_python_ast_reference_corpus() -> None:
    sources = {
        "pkg/control.py": """
@decorate(flag if enabled else fallback)
def decorated(value=(left if choose else right)):
    if value:
        work()
    elif (other if choose else fallback):
        work()
    elif third:
        work()
    else:
        work()
    try:
        with resource():
            while ready():
                work()
    except Error:
        work()
    match value:
        case 1:
            work()
    return value
""".lstrip(),
        "pkg/nested.py": """
def outer(values):
    for value in values:
        total = value + 1
    def nested(items):
        for item in items:
            output.append(item * 2)
        return output
    return nested(values)

def numeric_values(values):
    for value in values:
        total = value + 1
    return total

def register_routes():
    async def handler(request):
        return request
    return handler

def register_indirect():
    if enabled:
        def handler(request):
            return request
    return enabled
""".lstrip(),
        "pkg/async_star.py": """
async def consume(stream):
    async for value in stream:
        total += value
    async with lock:
        if ready:
            return total

def exception_group():
    try:
        work()
    except* ValueError:
        recover()
""".lstrip(),
    }
    records = list(sources.items())
    actual = json.loads(guardrail_ast_metrics_native(records))
    expected = [_reference_metrics(path, source) for path, source in records]
    assert actual == expected


def test_policy_thresholds_markers_allowlists_and_order(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # A private copy of the host allowlist: the module holds no global any more,
    # `_load_allowlist` resolves it per call, so the test owns the dict it mutates.
    allowlist = guardrail_audit._load_allowlist(guardrail_audit.ROOT)

    def native_issues(path: str, source: str) -> list[dict[str, object]]:
        policy = json.dumps(
            {
                "god_functions": sorted(allowlist["god_functions"]),
                "complexity": sorted(allowlist["complexity"]),
            }
        )
        payload = json.loads(guardrail_ast_metrics_native([(path, source)], policy))
        return payload[0]["issues"]

    source = "\n".join(
        ["def candidate(values):", "    # guardrail: allow-god-function"]
        + [f"    if flag_{index}: pass" for index in range(21)]
        + ["    for value in values:", "        output.append(value * 2)"]
        + ["    value = 1" for _ in range(77)]
    )
    issues = native_issues("pkg/probe.py", source)
    assert [issue["kind"] for issue in issues] == [
        "complexity",
        "native_hotspot_candidate",
    ]
    assert issues[0]["metric"] == {
        "branches": 22,
        "max_nesting": 1,
        "lineno": 1,
    }

    threshold_source = "\n".join(
        ["def boundary():", *("    value = 1" for _ in range(100))]
    )
    threshold_issues = native_issues("pkg/boundary.py", threshold_source)
    assert [issue["kind"] for issue in threshold_issues] == ["god_function"]
    assert threshold_issues[0]["metric"] == {"lines": 101, "lineno": 1}

    branch_source = "\n".join(
        ["def branchy():"]
        + [
            line
            for index in range(21)
            for line in (f"    if flag_{index}:", "        pass")
        ]
    )
    assert [
        issue["kind"] for issue in native_issues("pkg/branch.py", branch_source)
    ] == ["complexity"]
    nesting_source = "\n".join(
        ["def nested():"]
        + [f"{'    ' * (depth + 1)}if flag_{depth}:" for depth in range(6)]
        + [f"{'    ' * 7}return 1"]
    )
    assert [
        issue["kind"] for issue in native_issues("pkg/nesting.py", nesting_source)
    ] == ["complexity"]

    allowlist["complexity"].add("pkg/probe.py::candidate")
    assert not native_issues("pkg/probe.py", source)

    for entries in allowlist.values():
        entries.clear()
    route_source = "\n".join(
        ["def register_routes(values):", "    def handler():", "        return 1"]
        + [f"    if flag_{index}: pass" for index in range(21)]
        + ["    for value in values:", "        output.append(value * 2)"]
        + ["    value = 1" for _ in range(77)]
    )
    assert [
        issue["kind"]
        for issue in native_issues("pkg/routes.py", route_source)
        if issue["symbol"] == "register_routes"
    ] == ["native_hotspot_candidate"]


def test_god_file_boundary_and_cpython_syntax_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # ROOT under tmp_path carries no conductor/guardrail_allowlist.json, so the
    # per-call loader yields the empty allowlist this test wants.
    monkeypatch.setattr(guardrail_audit, "ROOT", tmp_path)
    exact = tmp_path / "exact.txt"
    exact.write_text("\n".join("line" for _ in range(1250)), encoding="utf-8")
    above = tmp_path / "above.txt"
    above.write_text("\n".join("line" for _ in range(1251)), encoding="utf-8")
    broken = tmp_path / "broken.py"
    broken.write_text("value = 1\ndef broken(:\n", encoding="utf-8")

    issues, python_count = guardrail_audit._structural_issues(
        [exact, above, broken], staged_only=False, from_ref=None
    )
    assert python_count == 1
    assert [(issue.kind, issue.path) for issue in issues] == [
        ("god_file", "above.txt"),
        ("syntax_error", "broken.py"),
    ]
    assert issues[0].metric == {"lines": 1251}
    assert issues[1].message == "Syntax error: invalid syntax"
    assert issues[1].metric == {"lineno": 2}


def test_nested_function_and_direct_route_semantics_drive_policy() -> None:
    source = """
def outer():
    def nested():
        if one:
            if two:
                if three:
                    if four:
                        if five:
                            if six:
                                return 1

def register_direct():
    def handler():
        return 1

def register_deep():
    if enabled:
        def handler():
            return 1

def registerish():
    def handler():
        return 1
""".lstrip()
    metrics = json.loads(guardrail_ast_metrics_native([("pkg/nested.py", source)]))[0]
    by_name = {function["symbol"]: function for function in metrics["functions"]}
    assert [function["symbol"] for function in metrics["functions"]] == [
        "outer",
        "nested",
        "register_direct",
        "handler",
        "register_deep",
        "handler",
        "registerish",
        "handler",
    ]
    assert by_name["outer"]["max_nesting"] == 6
    assert by_name["nested"]["max_nesting"] == 6
    assert by_name["register_direct"]["is_route_registration"] is True
    assert by_name["register_deep"]["is_route_registration"] is False
    assert by_name["registerish"]["is_route_registration"] is False
