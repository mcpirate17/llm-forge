"""Structural quality checks: lifecycle, shared state, abstraction, and configuration.

These are the rules no analyzer already in the toolchain can express. Ruff, Semgrep,
Bandit and cppcheck all reason inside one file at a time; every rule here needs either
the whole candidate tree (is this abstraction implemented anywhere? does this config key
carry a different default elsewhere? do two modules take the same two locks in opposite
order?) or exception-path reachability that a pattern matcher cannot see.

The check parses every Python module in the candidate snapshot to build its indices but
reports ONLY on files the candidate changed, the same shape as
``check_duplicate_function_bodies``. A pre-existing defect in an untouched module is not
this candidate's regression.

Severities are calibrated against a full-tree measurement over 2998 tracked modules
(2026-08-31), not guessed. ``inconsistent-lock-order`` and
``single-implementation-abstraction`` found 0; ``cleanup-not-on-failure-path`` found 5
and ``duplicate-config-default`` 6 (two keys, three sites each), and every one of those
eleven was hand-verified as a real defect. Those four rules block.
``unbounded-module-cache`` (33) and ``connection-close-not-on-failure-path`` (149) are
real but too populous to block, so they report at MEDIUM until the debt is paid down.
"""

from __future__ import annotations

import ast
import time
from collections.abc import Iterable, Iterator
from pathlib import Path

from conductor.candidate_review.checks import ReviewContext, _changed_files, _result
from conductor.candidate_review.model import CheckResult, Finding, Severity

CHECK_ID = "structure-audit"
SKIP_DIRECTORIES = frozenset({".venv", "node_modules", "__pycache__", ".run", ".git"})

# Growth without eviction is the defect; a bounded container is not.
GROWTH_METHODS = frozenset(
    {"append", "extend", "add", "update", "setdefault", "insert"}
)
EVICTION_METHODS = frozenset(
    {"pop", "popitem", "clear", "remove", "discard", "popleft"}
)
GROWABLE_FACTORIES = frozenset(
    {"dict", "list", "set", "defaultdict", "OrderedDict", "deque", "Counter"}
)
BOUND_KEYWORDS = frozenset({"maxlen", "maxsize"})

# Acquire -> the release that must be structurally guaranteed. Deliberately narrow:
# start/join and register/unregister were measured and are covered by the advisory
# Semgrep rules instead, because their release is routinely and correctly delegated to
# a caller rather than owned by the acquiring frame.
#
# The split between the two maps is a precision measurement, not a taxonomy. Locks and
# file handles leak 5 times across the tracked tree and every one is a real defect, so
# they block. Database connections leak 149 times, almost all of them sqlite3.connect()
# in a short-lived script or test where the process exit is the reclaim -- real, but not
# worth blocking a merge over, so they report at MEDIUM under their own rule id.
OWNED_LIFECYCLE_PAIRS = {"acquire": "release", "open": "close"}
CONNECTION_LIFECYCLE_PAIRS = {"connect": "close"}
LIFECYCLE_PAIRS = {**OWNED_LIFECYCLE_PAIRS, **CONNECTION_LIFECYCLE_PAIRS}
RELEASE_METHODS = frozenset(LIFECYCLE_PAIRS.values())

LOCK_TOKENS = ("lock", "mutex", "semaphore")
ENV_READERS = frozenset({"os.getenv", "os.environ.get"})

Site = tuple[str, int]
Abstraction = tuple[str, int, int]
ConfigSite = tuple[str, int, str]


def _is_lock_expression(text: str) -> bool:
    lowered = text.lower()
    return any(token in lowered for token in LOCK_TOKENS)


def _iter_snapshot_modules(snapshot: Path) -> Iterator[tuple[str, ast.Module]]:
    """Yield (relative path, parsed module) for every readable module in the snapshot.

    Unparseable modules are skipped in silence on purpose: ``python-ast`` already emits
    a CRITICAL ``python-parse`` for any changed file that will not parse, and an
    unchanged file that will not parse is not this candidate's problem.
    """

    for path in sorted(snapshot.rglob("*.py")):
        if SKIP_DIRECTORIES.intersection(path.parts):
            continue
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, SyntaxError, ValueError, RecursionError):
            continue
        yield path.relative_to(snapshot).as_posix(), tree


def _functions(tree: ast.AST) -> list[ast.FunctionDef | ast.AsyncFunctionDef]:
    return [
        node
        for node in ast.walk(tree)
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
    ]


def _receiver(func: ast.expr) -> tuple[str, str] | None:
    """(name, attribute) for ``name.attribute(...)``, else None."""

    if isinstance(func, ast.Attribute) and isinstance(func.value, ast.Name):
        return func.value.id, func.attr
    return None


def _subscript_name(node: ast.expr, names: frozenset[str]) -> str | None:
    if (
        isinstance(node, ast.Subscript)
        and isinstance(node.value, ast.Name)
        and node.value.id in names
    ):
        return node.value.id
    return None


def _is_unbounded_container(value: ast.expr) -> bool:
    if isinstance(value, ast.Dict):
        return not value.keys
    if isinstance(value, (ast.List, ast.Set)):
        return not value.elts
    if isinstance(value, ast.Call):
        name = (
            value.func.attr
            if isinstance(value.func, ast.Attribute)
            else getattr(value.func, "id", "")
        )
        if name not in GROWABLE_FACTORIES:
            return False
        bounded = any(keyword.arg in BOUND_KEYWORDS for keyword in value.keywords)
        return not value.args and not bounded
    return False


def _module_level_containers(tree: ast.Module) -> dict[str, int]:
    """Module-scope names bound to an empty, unbounded mutable container."""

    found: dict[str, int] = {}
    for node in tree.body:
        if isinstance(node, ast.Assign):
            targets: list[ast.expr] = list(node.targets)
            value: ast.expr | None = node.value
        elif isinstance(node, ast.AnnAssign):
            targets, value = [node.target], node.value
        else:
            continue
        if value is None or not _is_unbounded_container(value):
            continue
        for target in targets:
            if isinstance(target, ast.Name):
                found[target.id] = node.lineno
    return found


def _container_usage(
    tree: ast.Module, names: frozenset[str]
) -> tuple[dict[str, int], set[str]]:
    """Runtime growth sites and eviction evidence for module-scope containers.

    Only mutations inside a function body count as growth. A module that fills a
    registry at import time -- ``BUILTIN_CHECKS["native-source"] = ...`` -- is bounded
    by the number of modules, which is not the unbounded-state defect.
    """

    grown: dict[str, int] = {}
    evicted: set[str] = set()
    for function in _functions(tree):
        for node in ast.walk(function):
            if isinstance(node, ast.Call):
                target = _receiver(node.func)
                if target is None or target[0] not in names:
                    continue
                name, method = target
                if method in GROWTH_METHODS:
                    grown.setdefault(name, node.lineno)
                elif method in EVICTION_METHODS:
                    evicted.add(name)
            elif isinstance(node, ast.Assign):
                for assigned in node.targets:
                    grown_name = _subscript_name(assigned, names)
                    if grown_name is not None:
                        grown.setdefault(grown_name, node.lineno)
            elif isinstance(node, ast.Delete):
                for deleted in node.targets:
                    evicted_name = _subscript_name(deleted, names)
                    if evicted_name is not None:
                        evicted.add(evicted_name)
    return grown, evicted


def _unbounded_state_findings(rel: str, tree: ast.Module) -> list[Finding]:
    containers = _module_level_containers(tree)
    if not containers:
        return []
    grown, evicted = _container_usage(tree, frozenset(containers))
    return [
        Finding(
            check_id=CHECK_ID,
            rule_id="unbounded-module-cache",
            severity=Severity.MEDIUM,
            path=rel,
            line=line,
            message=(
                f"module-level {name!r} (defined line {containers[name]}) grows at "
                "runtime with no eviction, bound or maximum size"
            ),
            help="Bound the container (maxsize/maxlen) or add an explicit eviction path.",
            evidence={"container": name, "defined_line": containers[name]},
        )
        for name, line in sorted(grown.items())
        if name not in evicted
    ]


def _with_bound_names(function: ast.AST) -> set[str]:
    """Names whose lifetime a ``with`` statement already guarantees."""

    bound: set[str] = set()
    for node in ast.walk(function):
        if not isinstance(node, (ast.With, ast.AsyncWith)):
            continue
        for item in node.items:
            expression = item.context_expr
            if isinstance(expression, ast.Name):
                bound.add(expression.id)
            elif isinstance(expression, ast.Call):
                target = _receiver(expression.func)
                if target is not None:
                    bound.add(target[0])
            if isinstance(item.optional_vars, ast.Name):
                bound.add(item.optional_vars.id)
    return bound


def _finally_node_ids(function: ast.AST) -> set[int]:
    guarded: set[int] = set()
    for node in ast.walk(function):
        if isinstance(node, ast.Try):
            for statement in node.finalbody:
                guarded.update(id(inner) for inner in ast.walk(statement))
    return guarded


def _acquisitions(function: ast.AST) -> dict[str, tuple[str, int]]:
    """Resource name -> (acquiring verb, line).

    The resource is the name that *owns* the handle, which is not always the call's
    receiver. ``lock.acquire()`` owns through its receiver; ``fd = os.open(path)`` owns
    through the assignment target. Keying on the receiver alone would miss the second
    form entirely -- ``os`` is a module, not a resource.
    """

    acquired: dict[str, tuple[str, int]] = {}
    for node in ast.walk(function):
        if isinstance(node, ast.Call):
            target = _receiver(node.func)
            if target is not None and target[1] == "acquire":
                acquired.setdefault(target[0], ("acquire", node.lineno))
        elif isinstance(node, ast.Assign) and len(node.targets) == 1:
            owner = node.targets[0]
            call = node.value
            if not isinstance(owner, ast.Name) or not isinstance(call, ast.Call):
                continue
            verb = _acquiring_verb(call.func)
            if verb is not None:
                acquired.setdefault(owner.id, (verb, node.lineno))
    return acquired


def _acquiring_verb(func: ast.expr) -> str | None:
    name = func.attr if isinstance(func, ast.Attribute) else getattr(func, "id", "")
    if name in {"open", "connect"}:
        return name
    return None


def _releases(
    function: ast.AST, guarded: set[int]
) -> dict[str, list[tuple[str, bool]]]:
    """Resource name -> [(releasing verb, is it inside a finally)].

    Both spellings of the release count: ``handle.close()`` names the resource as the
    receiver, ``os.close(fd)`` names it as the first argument.
    """

    released: dict[str, list[tuple[str, bool]]] = {}
    for node in ast.walk(function):
        if not isinstance(node, ast.Call):
            continue
        verb = (
            node.func.attr
            if isinstance(node.func, ast.Attribute)
            else getattr(node.func, "id", "")
        )
        if verb not in RELEASE_METHODS:
            continue
        in_finally = id(node) in guarded
        target = _receiver(node.func)
        if target is not None:
            released.setdefault(target[0], []).append((verb, in_finally))
        for argument in node.args:
            if isinstance(argument, ast.Name):
                released.setdefault(argument.id, []).append((verb, in_finally))
    return released


def _cleanup_findings(rel: str, tree: ast.Module) -> list[Finding]:
    findings: list[Finding] = []
    for function in _functions(tree):
        guarded = _finally_node_ids(function)
        findings.extend(
            _leak_findings(
                rel,
                function,
                _acquisitions(function),
                _releases(function, guarded),
                _with_bound_names(function),
            )
        )
    return findings


def _leak_findings(
    rel: str,
    function: ast.FunctionDef | ast.AsyncFunctionDef,
    acquired: dict[str, tuple[str, int]],
    released: dict[str, list[tuple[str, bool]]],
    bound: set[str],
) -> list[Finding]:
    findings: list[Finding] = []
    for name, (method, line) in sorted(acquired.items()):
        if name in bound:
            continue
        expected = LIFECYCLE_PAIRS[method]
        matching = [entry for entry in released.get(name, []) if entry[0] == expected]
        if not matching or any(in_finally for _, in_finally in matching):
            continue
        owned = method in OWNED_LIFECYCLE_PAIRS
        findings.append(
            Finding(
                check_id=CHECK_ID,
                rule_id=(
                    "cleanup-not-on-failure-path"
                    if owned
                    else "connection-close-not-on-failure-path"
                ),
                severity=Severity.HIGH if owned else Severity.MEDIUM,
                path=rel,
                line=line,
                message=(
                    f"{name}.{method}() in {function.name!r} is released by "
                    f"{name}.{expected}() only on the success path; an exception "
                    "raised in between leaks it"
                ),
                help="Use a with-statement, or move the release into a try/finally.",
                evidence={"resource": name, "acquire": method, "release": expected},
            )
        )
    return findings


def _record_lock_orders(
    rel: str, tree: ast.Module, orders: dict[tuple[str, str], list[Site]]
) -> None:
    """Record every (outer, inner) lock pair a nested ``with`` establishes."""

    def walk(node: ast.AST, held: tuple[str, ...]) -> None:
        if isinstance(node, (ast.With, ast.AsyncWith)):
            taken = tuple(
                ast.unparse(item.context_expr)
                for item in node.items
                if _is_lock_expression(ast.unparse(item.context_expr))
            )
            for outer in held:
                for inner in taken:
                    if outer != inner:
                        orders.setdefault((outer, inner), []).append((rel, node.lineno))
            held = held + taken
        for child in ast.iter_child_nodes(node):
            walk(child, held)

    walk(tree, ())


def _lock_order_findings(
    changed: set[str], orders: dict[tuple[str, str], list[Site]]
) -> list[Finding]:
    findings: list[Finding] = []
    for (outer, inner), sites in sorted(orders.items()):
        opposite = orders.get((inner, outer))
        if not opposite:
            continue
        where = ", ".join(f"{path}:{number}" for path, number in opposite[:3])
        for rel, line in sites:
            if rel not in changed:
                continue
            findings.append(
                Finding(
                    check_id=CHECK_ID,
                    rule_id="inconsistent-lock-order",
                    severity=Severity.HIGH,
                    path=rel,
                    line=line,
                    message=(
                        f"takes {outer} before {inner}, but {where} takes them in the "
                        "opposite order; the two orderings can deadlock"
                    ),
                    help="Impose one global lock order, or take both under a single guard.",
                    evidence={"outer": outer, "inner": inner, "opposite_sites": where},
                )
            )
    return findings


def _is_abstract_method(node: ast.FunctionDef | ast.AsyncFunctionDef) -> bool:
    if any("abstractmethod" in ast.unparse(item) for item in node.decorator_list):
        return True
    body = node.body
    return (
        len(body) == 1
        and isinstance(body[0], ast.Raise)
        and "NotImplementedError" in ast.unparse(body[0])
    )


def _record_abstractions(
    rel: str,
    tree: ast.Module,
    declared: dict[str, Abstraction],
    subclasses: dict[str, set[str]],
) -> None:
    """Index abstract base classes, and every class that inherits from a name.

    ``Protocol`` subclasses are excluded on purpose: a Protocol is satisfied
    structurally, so having no nominal subclass is correct usage, not a needless layer.
    Measured: excluding Protocol removed all 9 false positives on the tracked tree.
    """

    for node in ast.walk(tree):
        if not isinstance(node, ast.ClassDef):
            continue
        bases = [ast.unparse(base).rsplit(".", 1)[-1] for base in node.bases]
        for base in bases:
            subclasses.setdefault(base, set()).add(f"{rel}::{node.name}")
        if "Protocol" in bases:
            continue
        methods = [
            member
            for member in node.body
            if isinstance(member, (ast.FunctionDef, ast.AsyncFunctionDef))
        ]
        if "ABC" in bases or any(_is_abstract_method(member) for member in methods):
            declared[node.name] = (rel, node.lineno, len(methods))


def _abstraction_findings(
    changed: set[str],
    declared: dict[str, Abstraction],
    subclasses: dict[str, set[str]],
) -> list[Finding]:
    findings: list[Finding] = []
    for name, (rel, line, methods) in sorted(declared.items()):
        if rel not in changed or methods < 2:
            continue
        implementations = subclasses.get(name, set())
        if len(implementations) > 1:
            continue
        findings.append(
            Finding(
                check_id=CHECK_ID,
                rule_id="single-implementation-abstraction",
                severity=Severity.HIGH,
                path=rel,
                line=line,
                message=(
                    f"abstract base {name!r} declares {methods} methods but has "
                    f"{len(implementations)} implementation(s) in the candidate tree"
                ),
                help="Collapse the interface into its one implementation, or use a Protocol.",
                evidence={
                    "abstraction": name,
                    "implementations": sorted(implementations),
                },
            )
        )
    return findings


def _record_config_defaults(
    rel: str, tree: ast.Module, keys: dict[str, list[ConfigSite]]
) -> None:
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call) or len(node.args) < 2:
            continue
        if ast.unparse(node.func) not in ENV_READERS:
            continue
        key = node.args[0]
        if not isinstance(key, ast.Constant) or not isinstance(key.value, str):
            continue
        keys.setdefault(key.value, []).append(
            (rel, node.lineno, ast.unparse(node.args[1]))
        )


def _config_findings(
    changed: set[str], keys: dict[str, list[ConfigSite]]
) -> list[Finding]:
    findings: list[Finding] = []
    for key, sites in sorted(keys.items()):
        defaults = {default for _, _, default in sites}
        if len(defaults) < 2:
            continue
        for rel, line, default in sites:
            if rel not in changed:
                continue
            others = sorted(defaults - {default})
            findings.append(
                Finding(
                    check_id=CHECK_ID,
                    rule_id="duplicate-config-default",
                    severity=Severity.HIGH,
                    path=rel,
                    line=line,
                    message=(
                        f"{key} defaults to {default} here but to {', '.join(others)} "
                        "elsewhere in the candidate tree"
                    ),
                    help="Read the key once into a shared constant and pass it down.",
                    evidence={"key": key, "default": default, "conflicting": others},
                )
            )
    return findings


def _sorted(findings: Iterable[Finding]) -> list[Finding]:
    return sorted(
        findings, key=lambda item: (item.path or "", item.line or 0, item.rule_id)
    )


def check_structure_audit(ctx: ReviewContext) -> CheckResult:
    """Cross-module lifecycle, shared-state, abstraction and configuration audit."""

    started = time.perf_counter()
    changed = set(_changed_files(ctx, {"python"}))
    if not changed:
        return _result(CHECK_ID, started)

    orders: dict[tuple[str, str], list[Site]] = {}
    declared: dict[str, Abstraction] = {}
    subclasses: dict[str, set[str]] = {}
    keys: dict[str, list[ConfigSite]] = {}
    findings: list[Finding] = []
    modules = 0

    for rel, tree in _iter_snapshot_modules(ctx.snapshot):
        modules += 1
        _record_lock_orders(rel, tree, orders)
        _record_abstractions(rel, tree, declared, subclasses)
        _record_config_defaults(rel, tree, keys)
        if rel in changed:
            findings.extend(_unbounded_state_findings(rel, tree))
            findings.extend(_cleanup_findings(rel, tree))

    findings.extend(_lock_order_findings(changed, orders))
    findings.extend(_abstraction_findings(changed, declared, subclasses))
    findings.extend(_config_findings(changed, keys))
    return _result(
        CHECK_ID,
        started,
        _sorted(findings),
        files=sorted(changed),
        metrics={"modules_indexed": modules},
    )
