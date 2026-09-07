from __future__ import annotations

import ast
import json
from pathlib import Path

import pytest

from conductor.reuse import core as slop_core
from conductor.reuse import detectors


def _allowlist(repo: Path) -> tuple[set[str], set[str]]:
    try:
        data = json.loads((repo / "conductor" / "guardrail_allowlist.json").read_text())
    except (OSError, json.JSONDecodeError):
        return set(), set()
    return set(data.get("god_files", [])), set(data.get("god_functions", []))


def _fallback_candidate(handler: ast.ExceptHandler, relative: str) -> dict | None:
    calls = [
        node
        for statement in handler.body
        for node in ast.walk(statement)
        if isinstance(node, ast.Call)
    ]
    has_raise = any(
        isinstance(node, ast.Raise)
        for statement in handler.body
        for node in ast.walk(statement)
    )
    signals = any(
        isinstance(call.func, ast.Name)
        and call.func.id in {"print", "warn"}
        or isinstance(call.func, ast.Attribute)
        and call.func.attr in {"critical", "error", "exception", "warning", "warn"}
        for call in calls
    )
    if has_raise or signals:
        return None
    controls = (ast.Pass, ast.Continue, ast.Break)
    control_only = all(isinstance(statement, controls) for statement in handler.body)
    broad = handler.type is None or any(
        isinstance(node, ast.Name) and node.id in {"BaseException", "Exception"}
        for node in ast.walk(handler.type)
    )
    silent_default = (
        broad
        and not calls
        and all(
            isinstance(statement, controls + (ast.Return, ast.Assign))
            for statement in handler.body
        )
    )
    if not control_only and not silent_default:
        return None
    if handler.type is None:
        confidence, value, severity = 0.98, 100, "critical"
    elif broad and control_only:
        confidence, value, severity = 0.90, 80, "high"
    elif broad:
        confidence, value, severity = 0.75, 45, "medium"
    else:
        confidence, value, severity = 0.65, 25, "medium"
    exception = ast.unparse(handler.type) if handler.type else "bare except"
    location = f"{relative}:{handler.lineno}"
    return {
        "id": f"fallback:{location}",
        "category": "silent_fallbacks",
        "severity": severity,
        "confidence": confidence,
        "value": value,
        "files": [relative],
        "location": location,
        "evidence": f"except {exception} has no raise, warning, or error log",
    }


def reference_scan(
    paths: list[Path], repo: Path
) -> tuple[int, int, list[str], list[str], list[dict], list[str]]:
    allowed_files, allowed_functions = _allowlist(repo)
    god_files: list[str] = []
    god_functions: list[str] = []
    fallbacks: list[dict] = []
    unparsable: list[str] = []
    for path in paths:
        try:
            source = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        relative = path.relative_to(repo).as_posix()
        lines = source.splitlines()
        line_count = source.count("\n") + 1
        if (
            line_count > detectors.GOD_FILE_LINES
            and relative not in allowed_files
            and "# guardrail: allow-god-file" not in source
        ):
            god_files.append(f"{path} ({line_count})")
        if path.suffix != ".py":
            continue
        try:
            tree = ast.parse(source, filename=str(path))
        except SyntaxError:
            unparsable.append(relative)
            continue
        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                end = node.end_lineno or node.lineno
                span = end - node.lineno + 1
                route_wrapper = node.name.startswith("register_") and any(
                    isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef))
                    for child in ast.iter_child_nodes(node)
                )
                key = f"{relative}::{node.name}"
                marker = "# guardrail: allow-god-function" in "\n".join(
                    lines[node.lineno - 1 : end]
                )
                if (
                    span > detectors.GOD_FUNC_LINES
                    and not route_wrapper
                    and key not in allowed_functions
                    and not marker
                ):
                    god_functions.append(f"{path}:{node.lineno} {node.name} ({span})")
            elif isinstance(node, ast.ExceptHandler):
                if candidate := _fallback_candidate(node, relative):
                    fallbacks.append(candidate)
    return (
        len(god_files),
        len(god_functions),
        god_files,
        god_functions,
        fallbacks,
        unparsable,
    )


def legacy_reference_scan(
    paths: list[Path], repo: Path
) -> tuple[int, int, list[str], list[str], list[dict], list[str]]:
    """The two independent file reads and AST walks used before the native batch."""
    allowed_files, allowed_functions = _allowlist(repo)
    god_files: list[str] = []
    god_functions: list[str] = []
    unparsable: list[str] = []
    for path in paths:
        try:
            source = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        relative = path.relative_to(repo).as_posix()
        lines = source.splitlines()
        line_count = source.count("\n") + 1
        if (
            line_count > detectors.GOD_FILE_LINES
            and relative not in allowed_files
            and "# guardrail: allow-god-file" not in source
        ):
            god_files.append(f"{path} ({line_count})")
        if path.suffix != ".py":
            continue
        try:
            tree = ast.parse(source, filename=str(path))
        except SyntaxError:
            unparsable.append(relative)
            continue
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            end = node.end_lineno or node.lineno
            span = end - node.lineno + 1
            route_wrapper = node.name.startswith("register_") and any(
                isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef))
                for child in ast.iter_child_nodes(node)
            )
            key = f"{relative}::{node.name}"
            marker = "# guardrail: allow-god-function" in "\n".join(
                lines[node.lineno - 1 : end]
            )
            if (
                span > detectors.GOD_FUNC_LINES
                and not route_wrapper
                and key not in allowed_functions
                and not marker
            ):
                god_functions.append(f"{path}:{node.lineno} {node.name} ({span})")

    fallbacks: list[dict] = []
    for path in paths:
        if path.suffix != ".py":
            continue
        try:
            source = path.read_text(encoding="utf-8", errors="replace")
            tree = ast.parse(source, filename=str(path))
        except (OSError, SyntaxError):
            continue
        relative = path.relative_to(repo).as_posix()
        fallbacks.extend(
            candidate
            for handler in ast.walk(tree)
            if isinstance(handler, ast.ExceptHandler)
            and (candidate := _fallback_candidate(handler, relative)) is not None
        )
    return (
        len(god_files),
        len(god_functions),
        god_files,
        god_functions,
        fallbacks,
        unparsable,
    )


def native_scan(paths: list[Path], repo: Path):
    allowed_files, allowed_functions = _allowlist(repo)
    return slop_core.audit_detector_scan(
        paths=[str(path) for path in paths],
        repo=str(repo),
        allowed_files=sorted(allowed_files),
        allowed_functions=sorted(allowed_functions),
        god_file_lines=detectors.GOD_FILE_LINES,
        god_func_lines=detectors.GOD_FUNC_LINES,
    )


def _long_function(name: str, *, marker: bool = False) -> str:
    body = [f"def {name}():"]
    if marker:
        body.append("    # guardrail: allow-god-function")
    body.extend("    pass" for _ in range(100 - int(marker)))
    return "\n".join(body) + "\n"


def test_native_detector_scan_matches_guardrail_boundaries_and_exemptions(
    tmp_path: Path,
) -> None:
    (tmp_path / "conductor").mkdir()
    (tmp_path / "conductor" / "guardrail_allowlist.json").write_text(
        json.dumps({"god_functions": ["allowed.py::allowed"], "god_files": []})
    )
    long_path = tmp_path / "long.py"
    long_path.write_text(_long_function("ordinary"))
    allowed = tmp_path / "allowed.py"
    allowed.write_text(_long_function("allowed"))
    marked = tmp_path / "marked.py"
    marked.write_text(_long_function("marked", marker=True))
    route = tmp_path / "route.py"
    route.write_text(
        "def register_routes():\n    def route():\n        pass\n" + "    pass\n" * 98
    )
    exact = tmp_path / "exact.rs"
    exact.write_text("x\n" * 1249)
    oversized = tmp_path / "oversized.rs"
    oversized.write_text("x\n" * 1250)
    exempt_file = tmp_path / "exempt.ts"
    exempt_file.write_text("// # guardrail: allow-god-file\n" + "x\n" * 1250)
    broken = tmp_path / "broken.py"
    broken.write_text("def broken(:\n" + "#\n" * 1250)
    missing = tmp_path / "missing.py"
    paths = [
        long_path,
        allowed,
        marked,
        route,
        exact,
        oversized,
        exempt_file,
        broken,
        missing,
    ]

    expected = reference_scan(paths, tmp_path)
    assert native_scan(paths, tmp_path) == expected
    assert expected[:2] == (2, 1)
    assert "ordinary (101)" in expected[3][0]


def test_native_detector_scan_matches_exception_handler_semantics(
    tmp_path: Path,
) -> None:
    path = tmp_path / "fallbacks.py"
    path.write_bytes(
        b"""\
def probe(value):
    try:
        return value()
    except:
        pass
    try:
        return value()
    except Exception:
        return None
    try:
        return value()
    except (ValueError, Exception):
        result = None
    try:
        return value()
    except ValueError:
        pass
    try:
        return value()
    except Exception:
        logger.exception('visible')
    try:
        return value()
    except BaseException:
        raise
# invalid byte is replaced during decoding: \xff
"""
    )
    expected = reference_scan([path], tmp_path)
    assert native_scan([path], tmp_path) == expected
    assert [item["confidence"] for item in expected[4]] == [0.98, 0.75, 0.75, 0.65]
    assert [item["severity"] for item in expected[4]] == [
        "critical",
        "medium",
        "medium",
        "medium",
    ]


def test_the_audit_refuses_to_report_a_scan_of_a_file_it_could_not_read(tmp_path):
    """The file most likely to be broken is the one an audit must not pass over.

    The scanner keeps reading -- an unparsable file still has lines, so the
    god-file check lands on it -- and hands back the files whose AST it never
    got. It is the audit boundary that has to refuse, because a caller counting
    findings cannot tell "no silent fallbacks in this file" from "this file was
    never parsed" unless somebody tells it.
    """
    (tmp_path / "conductor").mkdir()
    (tmp_path / "conductor" / "guardrail_allowlist.json").write_text(
        json.dumps({"god_functions": [], "god_files": []})
    )
    broken = tmp_path / "broken.py"
    broken.write_text("def run(:\n")
    assert native_scan([broken], tmp_path)[5] == ["broken.py"]
    with pytest.raises(ValueError, match="cannot parse"):
        detectors._detector_scan([broken], tmp_path)
