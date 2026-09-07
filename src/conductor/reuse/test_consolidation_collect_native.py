from __future__ import annotations

import ast
import copy
import hashlib
from dataclasses import asdict
from pathlib import Path

from conductor.reuse import consolidation


def _docstring(statement: ast.stmt) -> bool:
    return (
        isinstance(statement, ast.Expr)
        and isinstance(statement.value, ast.Constant)
        and isinstance(statement.value.value, str)
    )


def _without_docstrings(body: list[ast.stmt]) -> list[ast.stmt]:
    if body and _docstring(body[0]):
        body = body[1:]
    for statement in body:
        for child in ast.walk(statement):
            if (
                isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
                and child.body
                and _docstring(child.body[0])
            ):
                child.body = child.body[1:]
    return body


class _Bindings(ast.NodeVisitor):
    def __init__(self) -> None:
        self.names: set[str] = set()

    def visit_Name(self, node: ast.Name) -> None:
        if isinstance(node.ctx, (ast.Store, ast.Del)):
            self.names.add(node.id)

    def visit_arg(self, node: ast.arg) -> None:
        self.names.add(node.arg)

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self.names.add(node.name)

    visit_AsyncFunctionDef = visit_FunctionDef

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        self.names.add(node.name)

    def visit_Lambda(self, node: ast.Lambda) -> None:
        return

    def visit_Import(self, node: ast.Import) -> None:
        for alias in node.names:
            self.names.add(alias.asname or alias.name.split(".", 1)[0])

    def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
        for alias in node.names:
            self.names.add(alias.asname or alias.name)

    def visit_ExceptHandler(self, node: ast.ExceptHandler) -> None:
        if node.name:
            self.names.add(node.name)
        for statement in node.body:
            self.visit(statement)


class _RenameLocals(ast.NodeTransformer):
    def __init__(self, locals_: set[str]) -> None:
        self.locals = locals_
        self.names: dict[str, str] = {}

    def _name(self, value: str) -> str:
        return self.names.setdefault(value, f"_v{len(self.names)}")

    def visit_Name(self, node: ast.Name) -> ast.Name:
        if node.id in self.locals:
            node.id = self._name(node.id)
        return node

    def visit_arg(self, node: ast.arg) -> ast.arg:
        if node.arg in self.locals:
            node.arg = self._name(node.arg)
        self.generic_visit(node)
        return node

    def visit_FunctionDef(self, node: ast.FunctionDef) -> ast.FunctionDef:
        if node.name in self.locals:
            node.name = self._name(node.name)
        return node

    visit_AsyncFunctionDef = visit_FunctionDef

    def visit_ClassDef(self, node: ast.ClassDef) -> ast.ClassDef:
        if node.name in self.locals:
            node.name = self._name(node.name)
        return node

    def visit_Lambda(self, node: ast.Lambda) -> ast.Lambda:
        return node


def reference_normalize(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
) -> tuple[str, int]:
    body = _without_docstrings(copy.deepcopy(node.body))
    args = copy.deepcopy(node.args)
    bindings = _Bindings()
    for arg in (*args.posonlyargs, *args.args, *args.kwonlyargs):
        bindings.visit(arg)
    if args.vararg:
        bindings.visit(args.vararg)
    if args.kwarg:
        bindings.visit(args.kwarg)
    for statement in body:
        bindings.visit(statement)
    renamer = _RenameLocals(bindings.names)
    normalized_args = renamer.visit(args)
    normalized_body = [renamer.visit(statement) for statement in body]
    wrapper = ast.Module(body=normalized_body, type_ignores=[])
    canonical = "|".join(
        (
            type(node).__name__,
            ast.dump(normalized_args, annotate_fields=False),
            ast.dump(wrapper, annotate_fields=False),
            ast.dump(node.returns, annotate_fields=False) if node.returns else "",
        )
    )
    digest = hashlib.sha1(canonical.encode("utf-8"), usedforsecurity=False).hexdigest()
    return digest, sum(1 for _ in ast.walk(wrapper))


def reference_collect(
    paths: list[Path], repo: Path, min_lines: int
) -> tuple[list[consolidation.FuncRecord], int]:
    records: list[consolidation.FuncRecord] = []
    unparsable = 0
    for path in paths:
        try:
            source = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            unparsable += 1
            continue
        try:
            tree = ast.parse(source, filename=str(path))
        except SyntaxError:
            unparsable += 1
            continue
        relative = path.relative_to(repo).as_posix()
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            end = node.end_lineno or node.lineno
            if end - node.lineno + 1 < min_lines:
                continue
            digest, tokens = reference_normalize(node)
            records.append(
                consolidation.FuncRecord(
                    file=relative,
                    line_start=node.lineno,
                    line_end=end,
                    name=node.name,
                    node_hash=digest,
                    tokens=tokens,
                    source=ast.get_source_segment(source, node) or "",
                )
            )
    return records, unparsable


def _payload(
    result: tuple[list[consolidation.FuncRecord], int],
) -> tuple[list[dict], int]:
    records, unparsable = result
    return [asdict(record) for record in records], unparsable


def test_native_normalizer_matches_cpython_reference() -> None:
    source = '''\
def alpha(item: "Input", /, scale=2, *rest, enabled=True, **options) -> "Output":
    """outer documentation is not semantic"""
    import package.module as pm
    local = pm.convert(item, scale)
    try:
        result = global_call(local, literal=3)
    except Exception as problem:
        recovered = problem
        result = recovered
    def inner(value):
        """nested documentation is not semantic"""
        return value + local
    class Box:
        """class documentation is not semantic"""
        pass
    transform = lambda lambda_value: lambda_value + local
    return inner(result), Box, transform, rest, enabled, options

def beta(value, factor=2):
    renamed = value + factor
    return renamed
'''
    functions = [
        node
        for node in ast.walk(ast.parse(source))
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
    ]
    for function in functions:
        assert consolidation._normalize_hash(function) == reference_normalize(function)


def test_native_collect_preserves_boundary_async_and_breadth_first_order(
    tmp_path: Path,
) -> None:
    path = tmp_path / "order.py"
    path.write_text(
        "def outer(value):\n"
        "    first = value + 1\n"
        "    def nested(item):\n"
        "        return item * 2\n"
        "    second = nested(first)\n"
        "    return second\n\n"
        "async def later(value):\n"
        "    first = value + 1\n"
        "    second = first + 1\n"
        "    third = second + 1\n"
        "    fourth = third + 1\n"
        "    return fourth\n",
        encoding="utf-8",
    )
    expected = reference_collect([path], tmp_path, min_lines=6)
    actual = consolidation.collect_functions([path], tmp_path, min_lines=6)
    assert _payload(actual) == _payload(expected)
    assert [record.name for record in actual[0]] == ["outer", "later"]


def test_native_collect_preserves_text_decoding_and_failure_accounting(
    tmp_path: Path,
) -> None:
    valid = tmp_path / "encoded.py"
    valid.write_bytes(
        b"def encoded(value):\r\n"
        b"    text = 'caf\xc3\xa9'\r\n"
        b"    invalid = '\xff'\r\n"
        b"    combined = text + invalid\r\n"
        b"    result = combined + str(value)\r\n"
        b"    return result\r\n"
    )
    broken = tmp_path / "broken.py"
    broken.write_text("def broken(:\n", encoding="utf-8")
    missing = tmp_path / "missing.py"
    paths = [valid, broken, missing]
    expected = reference_collect(paths, tmp_path, min_lines=6)
    actual = consolidation.collect_functions(paths, tmp_path, min_lines=6)
    assert _payload(actual) == _payload(expected)
    assert actual[1] == 2
    assert "\N{LATIN SMALL LETTER E WITH ACUTE}" in actual[0][0].source
    assert "\ufffd" in actual[0][0].source
    assert "\r" not in actual[0][0].source
