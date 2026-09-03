"""Cross-module structural quality policy over facts extracted by Rust."""

from __future__ import annotations

import ast
import json
import time
from collections import defaultdict
from pathlib import Path
from typing import Any

from conductor._native import candidate_structure_facts_native
from conductor.candidate_review.checks import ReviewContext, _changed_files, _result
from conductor.candidate_review.model import CheckResult, Finding, Severity

CHECK_ID = "structure-audit"
SKIP_DIRECTORIES = frozenset({".venv", "node_modules", "__pycache__", ".run", ".git"})
GROWTH_METHODS = frozenset(
    {"append", "extend", "add", "update", "setdefault", "insert"}
)


def _snapshot_records(snapshot: Path) -> list[tuple[str, str]]:
    """Return readable Python sources in deterministic relative-path order."""

    records: list[tuple[str, str]] = []
    for path in sorted(snapshot.rglob("*.py")):
        if SKIP_DIRECTORIES.intersection(path.parts):
            continue
        try:
            source = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        records.append((path.relative_to(snapshot).as_posix(), source))
    return records


def _normalize_expression(source: str) -> str:
    """Keep legacy CPython spelling in diagnostics while Rust owns traversal."""

    return ast.unparse(ast.parse(source, mode="eval").body)


def _legacy_growth_lines(source: str, names: set[str]) -> dict[str, int]:
    """Recover CPython breadth-first tie-breaking for displayed growth lines only."""

    tree = ast.parse(source)
    grown: dict[str, int] = {}
    functions = (
        node
        for node in ast.walk(tree)
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
    )
    for function in functions:
        for node in ast.walk(function):
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
                receiver = node.func.value
                if (
                    isinstance(receiver, ast.Name)
                    and receiver.id in names
                    and node.func.attr in GROWTH_METHODS
                ):
                    grown.setdefault(receiver.id, node.lineno)
            elif isinstance(node, ast.Assign):
                for target in node.targets:
                    if (
                        isinstance(target, ast.Subscript)
                        and isinstance(target.value, ast.Name)
                        and target.value.id in names
                    ):
                        grown.setdefault(target.value.id, node.lineno)
    return grown


def _unbounded_finding(path: str, fact: dict[str, Any]) -> Finding:
    name = fact["name"]
    defined_line = fact["defined_line"]
    return Finding(
        check_id=CHECK_ID,
        rule_id="unbounded-module-cache",
        severity=Severity.MEDIUM,
        path=path,
        line=fact["growth_line"],
        message=(
            f"module-level {name!r} (defined line {defined_line}) grows at runtime "
            "with no eviction, bound or maximum size"
        ),
        help="Bound the container (maxsize/maxlen) or add an explicit eviction path.",
        evidence={"container": name, "defined_line": defined_line},
    )


def _leak_finding(path: str, fact: dict[str, Any]) -> Finding:
    owned = fact["owned"]
    resource = fact["resource"]
    acquire = fact["acquire"]
    release = fact["release"]
    return Finding(
        check_id=CHECK_ID,
        rule_id=(
            "cleanup-not-on-failure-path"
            if owned
            else "connection-close-not-on-failure-path"
        ),
        severity=Severity.HIGH if owned else Severity.MEDIUM,
        path=path,
        line=fact["line"],
        message=(
            f"{resource}.{acquire}() in {fact['function']!r} is released by "
            f"{resource}.{release}() only on the success path; an exception raised "
            "in between leaks it"
        ),
        help="Use a with-statement, or move the release into a try/finally.",
        evidence={"resource": resource, "acquire": acquire, "release": release},
    )


def _lock_findings(
    changed: set[str], orders: dict[tuple[str, str], list[tuple[str, int]]]
) -> list[Finding]:
    findings: list[Finding] = []
    for (outer, inner), sites in sorted(orders.items()):
        opposite = orders.get((inner, outer))
        if not opposite:
            continue
        where = ", ".join(f"{path}:{number}" for path, number in opposite[:3])
        for path, line in sites:
            if path not in changed:
                continue
            findings.append(
                Finding(
                    check_id=CHECK_ID,
                    rule_id="inconsistent-lock-order",
                    severity=Severity.HIGH,
                    path=path,
                    line=line,
                    message=(
                        f"takes {outer} before {inner}, but {where} takes them in the "
                        "opposite order; the two orderings can deadlock"
                    ),
                    help=(
                        "Impose one global lock order, or take both under a single guard."
                    ),
                    evidence={
                        "outer": outer,
                        "inner": inner,
                        "opposite_sites": where,
                    },
                )
            )
    return findings


def _abstraction_findings(
    changed: set[str],
    declared: dict[str, tuple[str, int, int]],
    subclasses: dict[str, set[str]],
) -> list[Finding]:
    findings: list[Finding] = []
    for name, (path, line, methods) in sorted(declared.items()):
        if path not in changed or methods < 2:
            continue
        implementations = subclasses.get(name, set())
        if len(implementations) > 1:
            continue
        findings.append(
            Finding(
                check_id=CHECK_ID,
                rule_id="single-implementation-abstraction",
                severity=Severity.HIGH,
                path=path,
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


def _config_findings(
    changed: set[str], keys: dict[str, list[tuple[str, int, str]]]
) -> list[Finding]:
    findings: list[Finding] = []
    for key, sites in sorted(keys.items()):
        defaults = {default for _, _, default in sites}
        if len(defaults) < 2:
            continue
        for path, line, default in sites:
            if path not in changed:
                continue
            others = sorted(defaults - {default})
            findings.append(
                Finding(
                    check_id=CHECK_ID,
                    rule_id="duplicate-config-default",
                    severity=Severity.HIGH,
                    path=path,
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


def check_structure_audit(ctx: ReviewContext) -> CheckResult:
    """Cross-module lifecycle, shared-state, abstraction and configuration audit."""

    started = time.perf_counter()
    changed = set(_changed_files(ctx, {"python"}))
    if not changed:
        return _result(CHECK_ID, started)

    records = _snapshot_records(ctx.snapshot)
    sources = dict(records)
    payload = json.loads(candidate_structure_facts_native(records, sorted(changed)))
    orders: dict[tuple[str, str], list[tuple[str, int]]] = defaultdict(list)
    declared: dict[str, tuple[str, int, int]] = {}
    subclasses: dict[str, set[str]] = defaultdict(set)
    keys: dict[str, list[tuple[str, int, str]]] = defaultdict(list)
    findings: list[Finding] = []

    for file_facts in payload["files"]:
        path = file_facts["path"]
        unbounded = file_facts["unbounded"]
        if unbounded:
            legacy_lines = _legacy_growth_lines(
                sources[path], {fact["name"] for fact in unbounded}
            )
            for fact in unbounded:
                fact["growth_line"] = legacy_lines[fact["name"]]
        findings.extend(_unbounded_finding(path, fact) for fact in unbounded)
        findings.extend(_leak_finding(path, fact) for fact in file_facts["leaks"])
        for fact in file_facts["lock_orders"]:
            pair = (
                _normalize_expression(fact["outer"]),
                _normalize_expression(fact["inner"]),
            )
            orders[pair].append((path, fact["line"]))
        for fact in file_facts["abstractions"]:
            declared[fact["name"]] = (path, fact["line"], fact["methods"])
        for fact in file_facts["subclasses"]:
            subclasses[fact["base"]].add(fact["implementation"])
        for fact in file_facts["configs"]:
            keys[fact["key"]].append(
                (path, fact["line"], _normalize_expression(fact["default"]))
            )

    findings.extend(_lock_findings(changed, orders))
    findings.extend(_abstraction_findings(changed, declared, subclasses))
    findings.extend(_config_findings(changed, keys))
    findings.sort(key=lambda item: (item.path or "", item.line or 0, item.rule_id))
    return _result(
        CHECK_ID,
        started,
        findings,
        files=sorted(changed),
        metrics={"modules_indexed": payload["modules_indexed"]},
    )
