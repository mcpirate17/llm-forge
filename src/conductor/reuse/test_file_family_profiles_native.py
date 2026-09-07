from __future__ import annotations

import ast
import copy
import hashlib
from collections import Counter, defaultdict
from pathlib import Path

from conductor.reuse import consolidation, file_families
from conductor.reuse import core as slop_core


class _ReferenceShapeNormalizer(ast.NodeTransformer):
    def visit_Name(self, node: ast.Name) -> ast.Name:
        node.id = "_name"
        return node

    def visit_arg(self, node: ast.arg) -> ast.arg:
        node.arg = "_arg"
        self.generic_visit(node)
        return node

    def visit_Constant(self, node: ast.Constant) -> ast.Constant:
        node.value = f"<const:{type(node.value).__name__}>"
        return node


def _call_name(node: ast.AST) -> str | None:
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        return node.attr
    return None


def _expanded(prefix: str, counts: Counter[str], cap: int = 6) -> set[str]:
    return {
        f"{prefix}:{feature}:{index}"
        for feature, count in counts.items()
        for index in range(1, min(count, cap) + 1)
    }


def _statement_shapes(tree: ast.AST) -> set[str]:
    selected = (
        ast.Assign,
        ast.AugAssign,
        ast.Expr,
        ast.For,
        ast.AsyncFor,
        ast.If,
        ast.Match,
        ast.Return,
        ast.Try,
        ast.While,
        ast.With,
        ast.AsyncWith,
    )
    counts: Counter[str] = Counter()
    for node in ast.walk(tree):
        if isinstance(node, selected):
            normalized = _ReferenceShapeNormalizer().visit(copy.deepcopy(node))
            dumped = ast.dump(normalized, annotate_fields=False)
            digest = hashlib.sha1(dumped.encode(), usedforsecurity=False).hexdigest()[
                :16
            ]
            counts[digest] += 1
    return _expanded("stmt", counts, cap=4)


def _structural_paths(tree: ast.AST) -> set[str]:
    counts: Counter[str] = Counter()

    def visit(node: ast.AST, parent: str = "", grandparent: str = "") -> None:
        kind = type(node).__name__
        if parent:
            counts[f"{parent}>{kind}"] += 1
        if grandparent:
            counts[f"{grandparent}>{parent}>{kind}"] += 1
        for child in ast.iter_child_nodes(node):
            visit(child, kind, parent)

    visit(tree)
    return _expanded("path", counts)


def _signature(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
) -> tuple[str, str]:
    args = node.args
    flags = (
        f"p{len(args.posonlyargs) + len(args.args)}:k{len(args.kwonlyargs)}:"
        f"v{int(args.vararg is not None)}:kw{int(args.kwarg is not None)}:"
        f"a{int(isinstance(node, ast.AsyncFunctionDef))}"
    )
    return f"api-name:{node.name}:{flags}", f"api-shape:{flags}"


def _schemas(tree: ast.AST) -> set[str]:
    features: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Dict):
            keys = sorted(
                key.value
                for key in node.keys
                if isinstance(key, ast.Constant) and isinstance(key.value, str)
            )
            if len(keys) >= 3:
                features.add("dict:" + ",".join(keys))
        elif isinstance(node, ast.Call):
            keywords = sorted(keyword.arg for keyword in node.keywords if keyword.arg)
            if len(keywords) >= 2:
                features.add(
                    f"ctor:{_call_name(node.func) or '?'}:" + ",".join(keywords)
                )
    return features


def reference_profile(path: Path, repo: Path) -> file_families.FileProfile | None:
    try:
        source = path.read_text(encoding="utf-8", errors="replace")
        tree = ast.parse(source, filename=str(path))
    except (OSError, SyntaxError):
        return None

    classes: set[str] = set()
    functions: set[str] = set()
    methods: set[str] = set()
    method_hashes: dict[str, set[str]] = defaultdict(set)
    api: set[str] = set()
    fields: set[str] = set()
    calls: set[str] = set()
    imports: set[str] = set()
    controls: Counter[str] = Counter()
    class_nodes = [node for node in ast.walk(tree) if isinstance(node, ast.ClassDef)]
    method_ids = {
        id(node)
        for class_node in class_nodes
        for node in class_node.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
    }
    for class_node in class_nodes:
        classes.add(class_node.name)
        api.add(f"class:{class_node.name}")
        for node in class_node.body:
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            methods.add(node.name)
            named, shaped = _signature(node)
            api.update((f"method:{named}", f"method:{shaped}"))
            method_hashes[node.name].add(consolidation._normalize_hash(node)[0])

    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            if id(node) not in method_ids:
                functions.add(node.name)
                named, shaped = _signature(node)
                api.update((f"function:{named}", f"function:{shaped}"))
        elif isinstance(node, ast.Attribute):
            if isinstance(node.value, ast.Name) and node.value.id in {"self", "cls"}:
                fields.add(f"field:{node.attr}")
        elif isinstance(node, ast.Call):
            if name := _call_name(node.func):
                calls.add(f"call:{name}")
        elif isinstance(node, ast.Import):
            imports.update(
                f"import:{alias.name.split('.', 1)[0]}" for alias in node.names
            )
        elif isinstance(node, ast.ImportFrom):
            if name := (node.module or "").split(".", 1)[0]:
                imports.add(f"import:{name}")
        elif isinstance(
            node,
            (
                ast.If,
                ast.For,
                ast.AsyncFor,
                ast.While,
                ast.Try,
                ast.Match,
                ast.With,
                ast.AsyncWith,
            ),
        ):
            controls[type(node).__name__] += 1

    return file_families.FileProfile(
        file=path.relative_to(repo).as_posix(),
        loc=source.count("\n") + 1,
        classes=frozenset(classes),
        function_names=frozenset(functions),
        method_names=frozenset(methods),
        method_hashes={
            name: frozenset(values) for name, values in method_hashes.items()
        },
        api=frozenset(api),
        fields=frozenset(fields),
        calls=frozenset(calls),
        control=frozenset(_expanded("control", controls, cap=8)),
        imports=frozenset(imports),
        structure=frozenset(_structural_paths(tree) | _statement_shapes(tree)),
        schemas=frozenset(_schemas(tree)),
    )


SOURCES = (
    """\
from alpha.beta import item
import gamma.delta as gd

class Lane:
    @decorator(flag=True, mode="fast")
    async def run(self, value: int = 1, *, scale=2, **options) -> None:
        payload = {"alpha": value, "beta": scale, "gamma": options}
        self.state = factory(value, scale=scale, mode="fast")
        async with manager() as handle:
            if payload:
                return None

        def nested(local):
            return local + 1

    def build(self) -> Product:
        from factory import Product

        return Product()
""",
    """\
def classify(value):
    match value:
        case {"kind": kind}:
            return kind
        case _:
            return "unknown"

try:
    RESULT = classify({"alpha": 1, "beta": True, "gamma": None})
except (TypeError, ValueError) as error:
    RESULT = str(error)
""",
    '''\
class UnicodeLane:
    def résumé(self, entrée="café"):
        """A docstring excluded from the normalized method hash."""
        sortie = entrée.strip()
        return sortie
''',
)


def test_native_profiles_match_cpython_reference_for_all_feature_fields(
    tmp_path: Path,
) -> None:
    for index, source in enumerate(SOURCES):
        path = tmp_path / f"case_{index}.py"
        path.write_text(source, encoding="utf-8")
        assert file_families.profile_file(path, tmp_path) == reference_profile(
            path, tmp_path
        )


def test_native_batch_preserves_order_and_counts_syntax_errors(tmp_path: Path) -> None:
    first = tmp_path / "first.py"
    broken = tmp_path / "broken.py"
    second = tmp_path / "second.py"
    first.write_text(SOURCES[0], encoding="utf-8")
    broken.write_text("def broken(:\n", encoding="utf-8")
    second.write_text(SOURCES[1], encoding="utf-8")

    raw, unparsable = slop_core.audit_file_family_profiles(
        paths=[str(first), str(broken), str(second)], repo=str(tmp_path)
    )

    assert [item["file"] for item in raw] == ["first.py", "second.py"]
    assert unparsable == 1


def test_native_profiles_preserve_invalid_utf8_replacement_semantics(
    tmp_path: Path,
) -> None:
    path = tmp_path / "replacement.py"
    path.write_bytes(b"VALUE = 'ok'\n# invalid: \xff\n")
    assert file_families.profile_file(path, tmp_path) == reference_profile(
        path, tmp_path
    )
