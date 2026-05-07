#!/usr/bin/env python3
from __future__ import annotations

import argparse
import ast
import json
import subprocess
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Iterable


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_TARGETS = ("research", "aria_core", "aria_designer")
ALLOWLIST_PATH = Path(__file__).resolve().parent / "guardrail_allowlist.json"


def _load_allowlist() -> dict[str, set[str]]:
    if not ALLOWLIST_PATH.exists():
        return {"god_files": set(), "god_functions": set(), "complexity": set()}
    try:
        raw = json.loads(ALLOWLIST_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {"god_files": set(), "god_functions": set(), "complexity": set()}
    return {
        "god_files": set(raw.get("god_files", [])),
        "god_functions": set(raw.get("god_functions", [])),
        "complexity": set(raw.get("complexity", [])),
    }


_ALLOWLIST = _load_allowlist()


def _has_marker(text: str, marker: str) -> bool:
    return f"# guardrail: {marker}" in text


def _function_has_marker(node: ast.AST, source_lines: list[str], marker: str) -> bool:
    start = getattr(node, "lineno", 1) - 1
    end = getattr(node, "end_lineno", start + 1)
    snippet = "\n".join(source_lines[max(start, 0) : min(end, len(source_lines))])
    return _has_marker(snippet, marker)


CODE_EXTS = {
    ".py",
    ".js",
    ".jsx",
    ".ts",
    ".tsx",
    ".c",
    ".cc",
    ".cpp",
    ".cu",
    ".h",
    ".hpp",
}
SKIP_PARTS = {
    "node_modules",
    ".venv",
    "__pycache__",
    ".git",
    "archive",
    ".pytest_cache",
    ".mypy_cache",
    "dist",
    "build",
    "conductor",
}


@dataclass
class Issue:
    kind: str
    severity: str
    path: str
    symbol: str | None
    message: str
    recommendation: str
    metric: dict[str, Any]


def _should_skip(path: Path) -> bool:
    return any(part in SKIP_PARTS for part in path.parts)


def _iter_files(targets: Iterable[str], staged_only: bool) -> list[Path]:
    if staged_only:
        proc = subprocess.run(
            ["git", "diff", "--cached", "--name-only", "--diff-filter=ACMR"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
        )
        candidates = [
            ROOT / line.strip() for line in proc.stdout.splitlines() if line.strip()
        ]
    else:
        candidates = []
        for target in targets:
            base = ROOT / target
            if not base.exists():
                continue
            candidates.extend(p for p in base.rglob("*") if p.is_file())
    out: list[Path] = []
    for path in candidates:
        if not path.exists() or _should_skip(path):
            continue
        if path.suffix.lower() in CODE_EXTS:
            out.append(path)
    return sorted(set(out))


class _PyFunctionAnalyzer(ast.NodeVisitor):
    def __init__(self, source_lines: list[str], rel_path: str = "") -> None:
        self.source_lines = source_lines
        self.rel_path = rel_path
        self.issues: list[Issue] = []
        self._parents: list[ast.AST] = []

    def generic_visit(self, node: ast.AST) -> None:
        self._parents.append(node)
        super().generic_visit(node)
        self._parents.pop()

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self._handle_function(node)

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        self._handle_function(node)

    def _handle_function(self, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
        end_lineno = getattr(node, "end_lineno", node.lineno)
        length = max(0, end_lineno - node.lineno + 1)
        branches = sum(
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
        )
        max_nesting = self._max_nesting(node)
        qualname = node.name

        # Flask/FastAPI route registration functions are structural wrappers
        # containing nested handler closures — not logic god functions.
        is_route_registration = qualname.startswith("register_") and any(
            isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef))
            for child in ast.iter_child_nodes(node)
        )

        fn_key = f"{self.rel_path}::{qualname}" if self.rel_path else qualname
        allow_god_fn = fn_key in _ALLOWLIST["god_functions"] or _function_has_marker(
            node, self.source_lines, "allow-god-function"
        )
        allow_complexity = fn_key in _ALLOWLIST["complexity"] or _function_has_marker(
            node, self.source_lines, "allow-complexity"
        )
        if length > 100 and not is_route_registration and not allow_god_fn:
            self.issues.append(
                Issue(
                    kind="god_function",
                    severity="critical",
                    path="",
                    symbol=qualname,
                    message=f"Function is {length} lines (>100).",
                    recommendation="Split by decision blocks and side-effect boundaries.",
                    metric={"lines": length, "lineno": node.lineno},
                )
            )
        if (
            (branches > 20 or max_nesting > 5)
            and not is_route_registration
            and not allow_complexity
        ):
            self.issues.append(
                Issue(
                    kind="complexity",
                    severity="high",
                    path="",
                    symbol=qualname,
                    message=f"Function complexity is high (branches={branches}, nesting={max_nesting}).",
                    recommendation="Flatten control flow and extract pure helpers.",
                    metric={
                        "branches": branches,
                        "max_nesting": max_nesting,
                        "lineno": node.lineno,
                    },
                )
            )
        if self._looks_like_python_hot_loop(node) and not allow_complexity:
            self.issues.append(
                Issue(
                    kind="native_hotspot_candidate",
                    severity="high",
                    path="",
                    symbol=qualname,
                    message="Python loop heuristic suggests a numeric hot path.",
                    recommendation="Vectorize with NumPy/PyTorch or move the hotspot into C/C++/Rust/Cython if profiling confirms it.",
                    metric={"lineno": node.lineno},
                )
            )
        self.generic_visit(node)

    def _max_nesting(self, fn: ast.AST) -> int:
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

        def walk(node: ast.AST, depth: int) -> int:
            best = depth
            for child in ast.iter_child_nodes(node):
                next_depth = depth + 1 if isinstance(child, control) else depth
                best = max(best, walk(child, next_depth))
            return best

        return walk(fn, 0)

    def _looks_like_python_hot_loop(self, node: ast.AST) -> bool:
        for child in ast.walk(node):
            if isinstance(child, (ast.For, ast.AsyncFor)):
                body_calls = [n for n in ast.walk(child) if isinstance(n, ast.Call)]
                has_append = any(
                    isinstance(call.func, ast.Attribute) and call.func.attr == "append"
                    for call in body_calls
                )
                has_numeric_op = any(
                    isinstance(n, (ast.BinOp, ast.AugAssign)) for n in ast.walk(child)
                )
                if has_append and has_numeric_op:
                    return True
                iter_name = getattr(child.iter, "id", "")
                if (
                    iter_name in {"x", "xs", "arr", "array", "tensor", "values"}
                    and has_numeric_op
                ):
                    return True
        return False


def _run_tool(command: list[str]) -> tuple[int, str]:
    try:
        proc = subprocess.run(
            command,
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
            timeout=120,
        )
    except FileNotFoundError:
        return 127, f"missing tool: {command[0]}"
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout or ""
        stderr = exc.stderr or ""
        return 124, f"timed out: {' '.join(command)}\n{stdout}\n{stderr}".strip()
    return proc.returncode, (proc.stdout + proc.stderr).strip()


def collect_issues(
    targets: Iterable[str], staged_only: bool = False
) -> tuple[list[Issue], dict[str, Any]]:
    issues: list[Issue] = []
    files = _iter_files(targets, staged_only=staged_only)
    file_count = 0
    py_count = 0
    for path in files:
        file_count += 1
        rel = path.relative_to(ROOT).as_posix()
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            text = path.read_text(encoding="latin-1")
        lines = text.splitlines()
        allow_god_file = rel in _ALLOWLIST["god_files"] or _has_marker(
            text, "allow-god-file"
        )
        if len(lines) > 1250 and not allow_god_file:
            issues.append(
                Issue(
                    kind="god_file",
                    severity="critical",
                    path=rel,
                    symbol=None,
                    message=f"File is {len(lines)} lines (>1250).",
                    recommendation="Split by responsibility boundaries and isolate orchestration from pure logic.",
                    metric={"lines": len(lines)},
                )
            )
        if path.suffix == ".py":
            py_count += 1
            try:
                tree = ast.parse(text, filename=rel)
            except SyntaxError as exc:
                issues.append(
                    Issue(
                        kind="syntax_error",
                        severity="critical",
                        path=rel,
                        symbol=None,
                        message=f"Syntax error: {exc.msg}",
                        recommendation="Fix parse errors before merge.",
                        metric={"lineno": exc.lineno},
                    )
                )
                continue
            analyzer = _PyFunctionAnalyzer(lines, rel_path=rel)
            analyzer.visit(tree)
            for issue in analyzer.issues:
                issue.path = rel
            issues.extend(analyzer.issues)

    dead_code_hits: list[str] = []
    duplicate_hits: list[str] = []
    vulture_rc = 0
    pylint_rc = 0
    if not staged_only:
        vulture_cmd = [
            "vulture",
            *targets,
            "vulture_whitelist.py",
            "--min-confidence",
            "80",
            "--exclude",
            "*/.venv/*,*/node_modules/*,*/__pycache__/*,*/.run/*,*/tests/*,*/migrations/*",
        ]
        vulture_rc, vulture_out = _run_tool(vulture_cmd)
        dead_code_hits = [
            line
            for line in vulture_out.splitlines()
            if line.strip() and "missing tool:" not in line
        ]
        for line in dead_code_hits[:25]:
            issues.append(
                Issue(
                    kind="dead_code",
                    severity="high",
                    path=line.split(":", 1)[0],
                    symbol=None,
                    message=line,
                    recommendation="Delete, wire in, or explicitly whitelist if intentionally dynamic.",
                    metric={},
                )
            )

        pylint_cmd = [
            "pylint",
            *targets,
            "--disable=all",
            "--enable=duplicate-code",
            "--min-similarity-lines=10",
        ]
        pylint_rc, pylint_out = _run_tool(pylint_cmd)
        duplicate_hits = [
            line for line in pylint_out.splitlines() if "duplicate-code" in line
        ]
        for line in duplicate_hits[:25]:
            issues.append(
                Issue(
                    kind="duplicate_code",
                    severity="medium",
                    path="multiple",
                    symbol=None,
                    message=line.strip(),
                    recommendation="Collapse repeated logic into one implementation or delete stale variants.",
                    metric={},
                )
            )

    return issues, {
        "files_scanned": file_count,
        "python_files_scanned": py_count,
        "vulture_exit_code": vulture_rc,
        "pylint_exit_code": pylint_rc,
        "dead_code_hits": len(dead_code_hits),
        "duplicate_hits": len(duplicate_hits),
    }


def _group(issues: list[Issue], *kinds: str) -> list[Issue]:
    order = {"critical": 0, "high": 1, "medium": 2, "low": 3}
    return sorted(
        [issue for issue in issues if issue.kind in kinds],
        key=lambda item: (order.get(item.severity, 9), item.path, item.symbol or ""),
    )


def build_markdown_report(issues: list[Issue], summary: dict[str, Any]) -> str:
    critical = _group(
        issues,
        "god_file",
        "god_function",
        "syntax_error",
        "native_hotspot_candidate",
        "dead_code",
        "complexity",
    )
    exact_targets = critical[:20]
    fast_wins = _group(issues, "dead_code", "duplicate_code", "complexity")[:10]
    structural = _group(issues, "god_file", "god_function")[:10]
    perf = _group(issues, "native_hotspot_candidate", "complexity")[:10]

    lines = [
        "# Audit Report",
        "",
        f"Scanned `{summary['files_scanned']}` code files across `{summary['python_files_scanned']}` Python files.",
        "",
        "### A. Critical problems",
    ]
    if critical:
        for issue in critical[:15]:
            symbol = f"::{issue.symbol}" if issue.symbol else ""
            lines.append(f"- `{issue.path}{symbol}` [{issue.severity}] {issue.message}")
    else:
        lines.append("- No critical guardrail violations detected.")

    lines.extend(["", "### B. Exact targets"])
    if exact_targets:
        for issue in exact_targets:
            lines.append(f"- file path: `{issue.path}`")
            lines.append(f"  symbol/function/class name: `{issue.symbol or '-'} `")
            lines.append(f"  estimated severity: `{issue.severity}`")
            lines.append(f"  why it is bad: {issue.message}")
            lines.append(f"  exact recommendation: {issue.recommendation}")
    else:
        lines.append("- No exact targets identified.")

    lines.extend(["", "### C. Fast wins"])
    if fast_wins:
        for issue in fast_wins:
            lines.append(f"- `{issue.path}`: {issue.recommendation}")
    else:
        lines.append("- No low-risk fast wins identified.")

    lines.extend(["", "### D. Structural rewrites"])
    if structural:
        for issue in structural:
            lines.append(f"- `{issue.path}`: {issue.message} {issue.recommendation}")
    else:
        lines.append("- No structural rewrites required by current thresholds.")

    lines.extend(
        [
            "",
            "### E. Performance upgrades by language",
            "- Python",
        ]
    )
    if perf:
        for issue in perf:
            lines.append(
                f"  - `{issue.path}` `{issue.symbol or ''}`: {issue.recommendation}"
            )
    else:
        lines.append(
            "  - No obvious Python hotspots were flagged by the current heuristic scan."
        )
    lines.extend(
        [
            "- JavaScript/TypeScript",
            "  - Add ESLint/unused-export enforcement next; this pass does not yet scan JS/TS symbol usage deeply.",
            "- Database/SQL",
            "  - No SQL-specific automated audit added in this pass; add query-plan/index checks separately.",
            "- Rust/C/C++/Cython opportunities",
            "  - Prioritize files flagged as `native_hotspot_candidate` after benchmark confirmation.",
            "",
            "### F. Proposed patch plan",
            "1. delete dead code",
            "2. split god files",
            "3. split god functions",
            "4. optimize hot paths",
            "5. optimize database access",
            "6. reduce dependency and bundle bloat",
            "7. move justified hotspots to compiled/native code",
            "8. benchmark before/after",
            "",
            "### G. Proof",
            f"- files scanned: {summary['files_scanned']}",
            f"- dead code hits reported by vulture: {summary['dead_code_hits']}",
            f"- duplicate-code hits reported by pylint: {summary['duplicate_hits']}",
            f"- critical findings: {sum(1 for issue in issues if issue.severity == 'critical')}",
            f"- high findings: {sum(1 for issue in issues if issue.severity == 'high')}",
        ]
    )
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Guardrail audit and blocking checks")
    parser.add_argument("--targets", nargs="*", default=list(DEFAULT_TARGETS))
    parser.add_argument("--staged-only", action="store_true")
    parser.add_argument(
        "--check", action="store_true", help="Exit non-zero on critical/high findings"
    )
    parser.add_argument("--markdown-out", type=str, default="")
    parser.add_argument("--json-out", type=str, default="")
    args = parser.parse_args()

    issues, summary = collect_issues(args.targets, staged_only=args.staged_only)
    payload = {"summary": summary, "issues": [asdict(issue) for issue in issues]}
    report = build_markdown_report(issues, summary)

    if args.markdown_out:
        out = ROOT / args.markdown_out
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(report, encoding="utf-8")
    else:
        sys.stdout.write(report)

    if args.json_out:
        out = ROOT / args.json_out
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    if args.check:
        blockers = [i for i in issues if i.severity == "critical"]
        return 1 if blockers else 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
