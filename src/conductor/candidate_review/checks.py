"""Built-in policy checks, graph-selected tests, and hermetic command execution."""

from __future__ import annotations

import ast
import fnmatch
import hashlib
import io
import json
import os
import re
import time
import tokenize
import tomllib
from dataclasses import dataclass
from datetime import UTC, datetime, timezone
from pathlib import Path
from typing import Callable, Iterable

from conductor.candidate_review.git_source import changed_line_numbers
from conductor.candidate_review.model import (
    Candidate,
    Change,
    CheckResult,
    CheckStatus,
    Finding,
    Severity,
    TreeEntry,
)
from conductor.candidate_review.ownership import OwnershipError, load_claims
from conductor.candidate_review.policy import CheckPolicy, Policy

SECRET_PATTERNS = {
    "private-key": re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----"),
    "aws-access-key": re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
    "github-token": re.compile(r"\bgh[psoru]_[A-Za-z0-9_]{30,}\b"),
    "generic-api-key": re.compile(
        r"(?i)(?:api[_-]?key|secret|token|password)\s*[:=]\s*['\"][A-Za-z0-9_./+=-]{20,}['\"]"
    ),
}
SOFTMAX_FALLBACK = re.compile(
    r"(?i)(?:fallback.{0,80}(?:softmax|attention)|(?:softmax|attention).{0,80}fallback|"
    r"scaled_dot_product_attention|MultiheadAttention)"
)
STUB_COMMENT = re.compile(r"\b(?:TODO|FIXME|XXX)\b|pragma:\s*no cover")
RANDOM_TEXT = re.compile(r"\b(?:random\.|np\.random|torch\.rand|torch\.randn)")
SEED_TEXT = re.compile(r"\b(?:seed|manual_seed|Generator)\b")


@dataclass(frozen=True, slots=True)
class ReviewContext:
    repo: Path
    snapshot: Path
    candidate: Candidate
    entries: tuple[TreeEntry, ...]
    policy: Policy
    surface: str
    profile: str
    owner: str | None
    runtime_dir: Path

    @property
    def classes(self) -> set[str]:
        return {item for change in self.candidate.changes for item in change.classes}

    @property
    def live_changes(self) -> tuple[Change, ...]:
        return tuple(change for change in self.candidate.changes if not change.deleted)


@dataclass(frozen=True, slots=True)
class TestSelection:
    tests: tuple[str, ...]
    graph: dict[str, object]
    findings: tuple[Finding, ...]


def _result(
    check_id: str,
    started: float,
    findings: Iterable[Finding] = (),
    *,
    files: Iterable[str] = (),
    metrics: dict[str, object] | None = None,
) -> CheckResult:
    items = [finding.finalize() for finding in findings]
    status = CheckStatus.FAILED if items else CheckStatus.PASSED
    return CheckResult(
        check_id=check_id,
        status=status,
        duration_ms=round((time.perf_counter() - started) * 1000),
        findings=items,
        files=sorted(set(files)),
        metrics=dict(metrics or {}),
    )


def _changed_files(ctx: ReviewContext, classes: Iterable[str] = ()) -> list[str]:
    selected = set(classes)
    return [
        change.path
        for change in ctx.live_changes
        if not selected or selected.intersection(change.classes)
    ]


def _tree_integrity_findings(
    ctx: ReviewContext,
) -> tuple[list[Finding], dict[str, TreeEntry]]:
    findings: list[Finding] = []
    folded_paths: dict[str, str] = {}
    entry_by_path = {entry.path: entry for entry in ctx.entries}
    for entry in ctx.entries:
        folded = entry.path.casefold()
        other = folded_paths.get(folded)
        if other and other != entry.path:
            findings.append(
                Finding(
                    check_id="candidate-integrity",
                    rule_id="case-collision",
                    severity=Severity.CRITICAL,
                    path=entry.path,
                    message=(
                        "case-colliding tree paths are not portable: "
                        f"{other!r} and {entry.path!r}"
                    ),
                )
            )
        folded_paths[folded] = entry.path
        if entry.mode not in {"100644", "100755", "120000", "160000"}:
            findings.append(
                Finding(
                    check_id="candidate-integrity",
                    rule_id="unsupported-git-mode",
                    severity=Severity.CRITICAL,
                    path=entry.path,
                    message=f"unsupported Git mode {entry.mode} in candidate tree",
                )
            )
    return findings, entry_by_path


def _protected_change_findings(ctx: ReviewContext, change: Change) -> list[Finding]:
    protected_old = change.old_path or change.path
    was_protected = any(
        fnmatch.fnmatchcase(protected_old, pattern)
        for pattern in ctx.policy.protected_delete_globs
    )
    stays_protected = any(
        fnmatch.fnmatchcase(change.path, pattern)
        for pattern in ctx.policy.protected_delete_globs
    )
    findings: list[Finding] = []
    if was_protected and (change.deleted or not stays_protected):
        findings.append(
            Finding(
                check_id="candidate-integrity",
                rule_id="protected-delete-or-move",
                severity=Severity.CRITICAL,
                path=protected_old,
                message=(
                    "protected artifact is deleted or moved out of protection: "
                    f"{protected_old}"
                ),
                evidence={"destination": None if change.deleted else change.path},
            )
        )
    if was_protected and change.status.startswith("M"):
        findings.append(
            Finding(
                check_id="candidate-integrity",
                rule_id="protected-overwrite",
                severity=Severity.HIGH,
                path=change.path,
                message="protected artifact overwrite requires a bound regeneration receipt",
            )
        )
    return findings


def _materialized_change_findings(
    ctx: ReviewContext,
    change: Change,
    entry_by_path: dict[str, TreeEntry],
) -> list[Finding]:
    if change.deleted:
        return []
    entry = entry_by_path.get(change.path)
    path = ctx.snapshot / change.path
    if entry is None or not os.path.lexists(path):
        return [
            Finding(
                check_id="candidate-integrity",
                rule_id="incomplete-snapshot",
                severity=Severity.CRITICAL,
                path=change.path,
                message="candidate path was not materialized from its Git object",
            )
        ]
    if entry.mode == "120000":
        return [
            Finding(
                check_id="candidate-integrity",
                rule_id="symlink-admission",
                severity=Severity.MEDIUM,
                path=change.path,
                message=(
                    "tracked symlink requires explicit review; "
                    "target is confined to the snapshot"
                ),
                evidence={"target": os.readlink(path)},
            )
        ]
    size = path.stat().st_size
    limit = (
        ctx.policy.max_binary_bytes
        if "binary" in change.classes
        else ctx.policy.max_file_bytes
    )
    findings: list[Finding] = []
    if size > limit:
        findings.append(
            Finding(
                check_id="candidate-integrity",
                rule_id="oversized-artifact",
                severity=Severity.HIGH,
                path=change.path,
                message=f"candidate file is {size} bytes; policy limit is {limit}",
                evidence={"size_bytes": size, "limit_bytes": limit},
            )
        )
    if "binary" in change.classes and change.status.startswith("A"):
        findings.append(
            Finding(
                check_id="candidate-integrity",
                rule_id="binary-admission",
                severity=Severity.HIGH,
                path=change.path,
                message=(
                    "new binary/model artifact is forbidden without "
                    "an owned narrow exception"
                ),
            )
        )
    return findings


def check_candidate_integrity(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    findings, entry_by_path = _tree_integrity_findings(ctx)
    if not ctx.candidate.changes and ctx.surface == "ci":
        findings.append(
            Finding(
                check_id="candidate-integrity",
                rule_id="empty-ci-range",
                severity=Severity.CRITICAL,
                message="CI candidate range is empty; refusing a no-op governance pass.",
                help="Resolve and pass an explicit merge-base-to-candidate range.",
            )
        )
    for change in ctx.candidate.changes:
        if change.new_mode == "160000":
            findings.append(
                Finding(
                    check_id="candidate-integrity",
                    rule_id="submodule-admission",
                    severity=Severity.HIGH,
                    path=change.path,
                    message=(
                        "new or modified submodule/gitlink requires "
                        "a narrow owned exception"
                    ),
                    evidence={"gitlink_oid": change.new_oid},
                )
            )
        findings.extend(_protected_change_findings(ctx, change))
        findings.extend(_materialized_change_findings(ctx, change, entry_by_path))
    return _result(
        "candidate-integrity",
        started,
        findings,
        files=[change.path for change in ctx.candidate.changes],
        metrics={
            "tree_entries": len(ctx.entries),
            "changes": len(ctx.candidate.changes),
        },
    )


def check_config_and_notebooks(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    findings: list[Finding] = []
    files = _changed_files(ctx, {"config", "notebook", "workflow"})
    yaml_module: object | None = None
    for rel in files:
        path = ctx.snapshot / rel
        suffix = path.suffix.lower()
        try:
            if suffix == ".toml":
                tomllib.loads(path.read_text(encoding="utf-8"))
            elif suffix in {".json", ".ipynb"}:
                payload = json.loads(path.read_text(encoding="utf-8"))
                if suffix == ".ipynb":
                    _validate_notebook(rel, payload, findings)
            elif suffix in {".yaml", ".yml"}:
                if yaml_module is None:
                    try:
                        import yaml as yaml_module  # type: ignore[import-not-found]
                    except ImportError as exc:
                        raise RuntimeError(
                            "PyYAML is required for YAML admission checks"
                        ) from exc
                yaml_module.safe_load(path.read_text(encoding="utf-8"))  # type: ignore[attr-defined]
        except Exception as exc:
            findings.append(
                Finding(
                    check_id="config-parse",
                    rule_id="malformed-config",
                    severity=Severity.CRITICAL,
                    path=rel,
                    message=f"candidate config/notebook cannot be parsed: {type(exc).__name__}: {exc}",
                )
            )
    return _result("config-parse", started, findings, files=files)


def _validate_notebook(rel: str, payload: object, findings: list[Finding]) -> None:
    if not isinstance(payload, dict) or not isinstance(payload.get("cells"), list):
        raise ValueError("notebook must be an object with a cells array")
    if payload.get("nbformat") != 4:
        raise ValueError(f"unsupported notebook format: {payload.get('nbformat')!r}")
    for index, cell in enumerate(payload["cells"]):
        if not isinstance(cell, dict):
            raise ValueError(f"cell {index} is not an object")
        if cell.get("cell_type") == "code" and (
            cell.get("outputs") or cell.get("execution_count")
        ):
            findings.append(
                Finding(
                    check_id="config-parse",
                    rule_id="notebook-output",
                    severity=Severity.HIGH,
                    path=rel,
                    line=index + 1,
                    message="tracked notebooks must have outputs and execution counts stripped",
                )
            )


def check_secrets(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    findings: list[Finding] = []
    files: list[str] = []
    for change in ctx.live_changes:
        path = ctx.snapshot / change.path
        if (
            path.is_symlink()
            or not path.is_file()
            or path.stat().st_size > ctx.policy.max_file_bytes
        ):
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        files.append(change.path)
        for rule_id, pattern in SECRET_PATTERNS.items():
            for match in pattern.finditer(text):
                findings.append(
                    Finding(
                        check_id="secret-scan",
                        rule_id=rule_id,
                        severity=Severity.CRITICAL,
                        path=change.path,
                        line=text.count("\n", 0, match.start()) + 1,
                        message="candidate contains secret-like credential material",
                        help="Remove and rotate the credential; do not baseline live secrets.",
                    )
                )
    return _result("secret-scan", started, findings, files=files)


class _PythonVisitor(ast.NodeVisitor):
    def __init__(self, rel: str, lines: list[str], hot_path: bool) -> None:
        self.rel = rel
        self.lines = lines
        self.hot_path = hot_path
        self.findings: list[Finding] = []
        self.loop_depth = 0
        self.protocol_depth = 0

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self._visit_function(node)

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        self._visit_function(node)

    def _visit_function(self, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
        length = (node.end_lineno or node.lineno) - node.lineno + 1
        if length > 100:
            self._add(
                "oversized-function",
                Severity.HIGH,
                node,
                f"function is {length} lines (>100)",
            )
        body = node.body
        if len(body) == 1 and isinstance(body[0], ast.Pass):
            self._add(
                "pass-stub",
                Severity.HIGH,
                body[0],
                "pass-only function is a partial implementation",
            )
        if (
            len(body) == 1
            and isinstance(body[0], ast.Expr)
            and isinstance(body[0].value, ast.Constant)
        ):
            if body[0].value.value is Ellipsis and not self.protocol_depth:
                self._add(
                    "ellipsis-stub",
                    Severity.HIGH,
                    body[0],
                    "ellipsis-only function is a stub",
                )
        self.generic_visit(node)

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        enclosing = self.protocol_depth
        if any(
            _call_name(base).rsplit(".", 1)[-1] == "Protocol" for base in node.bases
        ):
            self.protocol_depth = 1
        else:
            self.protocol_depth = 0
        self.generic_visit(node)
        self.protocol_depth = enclosing

    def visit_For(self, node: ast.For) -> None:
        self.loop_depth += 1
        if self.loop_depth >= 2 and self.hot_path:
            self._add(
                "nested-loop-hotpath",
                Severity.MEDIUM,
                node,
                "nested Python loop in a high-risk path needs a measured complexity budget or vectorized/native path",
            )
        self.generic_visit(node)
        self.loop_depth -= 1

    def visit_While(self, node: ast.While) -> None:
        self.loop_depth += 1
        self.generic_visit(node)
        self.loop_depth -= 1

    def visit_Call(self, node: ast.Call) -> None:
        name = _call_name(node.func)
        if isinstance(node.func, ast.Name) and node.func.id in {"eval", "exec"}:
            flagged = node.func.id
        elif name == "os.system":
            flagged = name
        else:
            flagged = None
        if flagged is not None:
            self._add(
                "dynamic-execution",
                Severity.CRITICAL,
                node,
                f"unsafe dynamic execution via {flagged}",
            )
        if name in {"pickle.load", "pickle.loads", "dill.load", "dill.loads"}:
            self._add(
                "unsafe-deserialization",
                Severity.CRITICAL,
                node,
                f"unsafe deserialization via {name}",
            )
        if name == "yaml.load" and not any(
            keyword.arg == "Loader" for keyword in node.keywords
        ):
            self._add(
                "unsafe-yaml",
                Severity.CRITICAL,
                node,
                "yaml.load without an explicit safe loader",
            )
        if name in {
            "subprocess.run",
            "subprocess.call",
            "subprocess.Popen",
            "os.popen",
        }:
            shell_true = any(
                keyword.arg == "shell"
                and isinstance(keyword.value, ast.Constant)
                and keyword.value.value is True
                for keyword in node.keywords
            )
            if shell_true or name == "os.popen":
                self._add(
                    "unsafe-shell",
                    Severity.HIGH,
                    node,
                    "shell execution is injection-prone",
                )
        repeated_in_nested_loop = self.loop_depth >= 2 and name in {
            "json.load",
            "json.loads",
            "Path.read_text",
            "Path.read_bytes",
            "open",
        }
        repeated_compile = self.loop_depth >= 1 and name == "re.compile"
        if repeated_in_nested_loop or repeated_compile:
            self._add(
                "repeated-parsing-io",
                Severity.HIGH if self.hot_path else Severity.MEDIUM,
                node,
                f"{name} inside a loop causes repeated parsing or I/O",
            )
        self.generic_visit(node)

    def visit_Raise(self, node: ast.Raise) -> None:
        raised = node.exc.func if isinstance(node.exc, ast.Call) else node.exc
        if raised is not None and _call_name(raised) == "NotImplementedError":
            self._add(
                "not-implemented-stub",
                Severity.HIGH,
                node,
                "NotImplementedError is a partial implementation",
            )
        self.generic_visit(node)

    def _add(self, rule: str, severity: Severity, node: ast.AST, message: str) -> None:
        self.findings.append(
            Finding(
                check_id="python-ast",
                rule_id=rule,
                severity=severity,
                path=self.rel,
                line=getattr(node, "lineno", None),
                column=getattr(node, "col_offset", None),
                message=message,
            )
        )


def _call_name(node: ast.expr) -> str:
    """Resolve a call target to its dotted name, or "" when it cannot be resolved.

    An attribute whose receiver is not itself a resolvable dotted name — a call,
    subscript, literal — is deliberately NOT reported under its bare attribute
    name. ``model.to(device).eval()`` is ``torch.nn.Module.eval``, not the ``eval``
    builtin; returning ``"eval"`` for it raised a CRITICAL dynamic-execution
    finding on the standard PyTorch idiom and made every model-evaluation script
    in the repo untrackable.

    Every name this module matches (``eval``, ``exec``, ``os.system``,
    ``pickle.load``, ``yaml.load``, ``NotImplementedError``, …) is either a bare
    builtin — an ``ast.Name``, still resolved above — or dotted from a ``Name``
    root, so refusing the bare-attribute guess loses no real detection.
    """
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        prefix = _call_name(node.value)
        return f"{prefix}.{node.attr}" if prefix else ""
    return ""


def _changed_stub_comment_lines(text: str, changed: set[int]) -> list[int]:
    markers: list[int] = []
    for token in tokenize.generate_tokens(io.StringIO(text).readline):
        if (
            token.type == tokenize.COMMENT
            and token.start[0] in changed
            and STUB_COMMENT.search(token.string)
        ):
            markers.append(token.start[0])
    return markers


def _changed_text(lines: list[str], changed: set[int]) -> str:
    return "\n".join(
        lines[number - 1] for number in sorted(changed) if number <= len(lines)
    )


def check_python_ast(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    findings: list[Finding] = []
    files = _changed_files(ctx, {"python"})
    changes = {change.path: change for change in ctx.live_changes}
    line_map = changed_line_numbers(ctx.repo, ctx.candidate, files)
    for rel in files:
        path = ctx.snapshot / rel
        try:
            text = path.read_text(encoding="utf-8")
            tree = ast.parse(text, filename=rel)
        except (OSError, UnicodeDecodeError, SyntaxError) as exc:
            findings.append(
                Finding(
                    check_id="python-ast",
                    rule_id="python-parse",
                    severity=Severity.CRITICAL,
                    path=rel,
                    line=getattr(exc, "lineno", None),
                    message=f"candidate Python cannot be parsed: {exc}",
                )
            )
            continue
        lines = text.splitlines()
        if len(lines) > 1250:
            findings.append(
                Finding(
                    check_id="python-ast",
                    rule_id="oversized-module",
                    severity=Severity.HIGH,
                    path=rel,
                    message=f"module is {len(lines)} lines (>1250)",
                )
            )
        hot_path = any(
            fnmatch.fnmatchcase(rel, pattern) for pattern in ctx.policy.hot_path_globs
        )
        visitor = _PythonVisitor(rel, lines, hot_path)
        visitor.visit(tree)
        findings.extend(visitor.findings)
        changed_text = _changed_text(lines, line_map.get(rel, set()))
        stub_lines = _changed_stub_comment_lines(text, line_map.get(rel, set()))
        if "test" not in changes[rel].classes and stub_lines:
            findings.append(
                Finding(
                    check_id="python-ast",
                    rule_id="partial-implementation-marker",
                    severity=Severity.HIGH,
                    path=rel,
                    line=stub_lines[0],
                    message="changed production code contains TODO/stub/coverage-bypass scaffolding",
                )
            )
        if "novel" in changes[rel].classes and "test" not in changes[rel].classes:
            match = SOFTMAX_FALLBACK.search(changed_text)
            if match:
                findings.append(
                    Finding(
                        check_id="python-ast",
                        rule_id="softmax-shaped-fallback",
                        severity=Severity.CRITICAL,
                        path=rel,
                        line=changed_text.count("\n", 0, match.start()) + 1,
                        message=(
                            "novel-mechanism path adds an attention/softmax-shaped fallback; "
                            "fix the novel branch instead of masking it"
                        ),
                    )
                )
        if RANDOM_TEXT.search(changed_text) and not SEED_TEXT.search(text):
            findings.append(
                Finding(
                    check_id="python-ast",
                    rule_id="nondeterministic-research",
                    severity=Severity.HIGH,
                    path=rel,
                    message="randomized changed code has no explicit deterministic seed path",
                )
            )
        if (
            any(
                token in rel
                for token in ("promotion", "program_write", "result_record")
            )
            and "stage1_passed" in changed_text
            and not all(
                metric in text
                for metric in ("wikitext", "hellaswag", "blimp", "binding")
            )
        ):
            findings.append(
                Finding(
                    check_id="python-ast",
                    rule_id="partial-promotion-write",
                    severity=Severity.CRITICAL,
                    path=rel,
                    message="promotion/write path changes stage-1 success without the complete metric contract",
                )
            )
    return _result("python-ast", started, findings, files=files)


def check_dependency_integrity(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    changes = [
        change for change in ctx.candidate.changes if "dependency" in change.classes
    ]
    findings: list[Finding] = []
    paths = {change.path for change in changes}
    pairs = [
        ("pyproject.toml", "uv.lock"),
        ("package.json", "package-lock.json"),
        ("Cargo.toml", "Cargo.lock"),
    ]
    for manifest, lock in pairs:
        touched_manifests = [
            path for path in paths if path == manifest or path.endswith(f"/{manifest}")
        ]
        for touched in touched_manifests:
            prefix = touched[: -len(manifest)]
            expected = f"{prefix}{lock}"
            if expected not in paths and not (ctx.snapshot / expected).is_file():
                findings.append(
                    Finding(
                        check_id="dependency-integrity",
                        rule_id="missing-lockfile",
                        severity=Severity.CRITICAL,
                        path=touched,
                        message=f"dependency manifest has no candidate lockfile: {expected}",
                    )
                )
    return _result(
        "dependency-integrity",
        started,
        findings,
        files=paths,
        metrics={"dependency_files": len(paths)},
    )


def check_ownership(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    findings: list[Finding] = []
    relevant = [
        change.path
        for change in ctx.candidate.changes
        if not change.deleted
        and change.path != ".current_work.md"
        and (not change.classes or not set(change.classes).issubset({"docs", "config"}))
    ]
    if ctx.surface != "pre-commit":
        return _result(
            "ownership",
            started,
            files=relevant,
            metrics={"status": "local-precommit-only"},
        )
    try:
        claims, state_digest = load_claims(ctx.repo)
    except OwnershipError as exc:
        if relevant:
            findings.append(
                Finding(
                    check_id="ownership",
                    rule_id="malformed-claim-store",
                    severity=Severity.HIGH,
                    message=f"ownership claim evidence is invalid: {exc}",
                )
            )
        return _result("ownership", started, findings, files=relevant)
    now = datetime.now(timezone.utc)
    active = [claim for claim in claims if claim.active(now)]
    for claim in claims:
        if not claim.active(now):
            findings.append(
                Finding(
                    check_id="ownership",
                    rule_id="stale-claim",
                    severity=Severity.MEDIUM,
                    message=f"ownership claim {claim.claim_id} for {claim.owner} expired",
                    evidence={
                        "claim_id": claim.claim_id,
                        "owner": claim.owner,
                        "expires_at": claim.expires_at,
                    },
                )
            )
    for path in relevant:
        matching = [
            claim
            for claim in active
            if any(path == item or path.startswith(f"{item}/") for item in claim.paths)
        ]
        if not matching:
            findings.append(
                Finding(
                    check_id="ownership",
                    rule_id="unclaimed-path",
                    severity=Severity.HIGH,
                    path=path,
                    message="candidate path is not covered by an active exact ownership claim",
                    help="Create a narrow claim with candidate-review claim before editing or commit from an isolated worktree.",
                )
            )
        elif ctx.owner and not any(
            ctx.owner.casefold() == claim.owner.casefold() for claim in matching
        ):
            findings.append(
                Finding(
                    check_id="ownership",
                    rule_id="claim-owner-mismatch",
                    severity=Severity.HIGH,
                    path=path,
                    message=f"candidate owner {ctx.owner!r} does not own the active path claim",
                    evidence={"claim_owners": [claim.owner for claim in matching]},
                )
            )
    return _result(
        "ownership",
        started,
        findings,
        files=relevant,
        metrics={
            "active_claims": len(active),
            "claim_ids": [claim.claim_id for claim in active],
            "state_sha256": state_digest,
        },
    )


def check_mutation_evidence(ctx: ReviewContext) -> CheckResult:
    from conductor.candidate_review.verification import (
        check_mutation_evidence as verify_mutation_receipts,
    )

    return verify_mutation_receipts(ctx)


BUILTIN_CHECKS: dict[str, Callable[[ReviewContext], CheckResult]] = {
    "candidate-integrity": check_candidate_integrity,
    "config-parse": check_config_and_notebooks,
    "dependency-integrity": check_dependency_integrity,
    "mutation-evidence": check_mutation_evidence,
    "ownership": check_ownership,
    "python-ast": check_python_ast,
    "secret-scan": check_secrets,
}


def check_performance_evidence(
    ctx: ReviewContext, selection: TestSelection
) -> CheckResult:
    started = time.perf_counter()
    hot_changes = [
        change
        for change in ctx.live_changes
        if any(
            fnmatch.fnmatchcase(change.path, pattern)
            for pattern in ctx.policy.hot_path_globs
        )
        and "test" not in change.classes
    ]
    if not hot_changes:
        return _result("performance-evidence", started)
    evidence_paths = [
        change.path
        for change in ctx.live_changes
        if re.search(r"(?i)(?:bench|perf|throughput|complexity|memory)", change.path)
    ]
    evidence_text = ""
    for test in selection.tests:
        try:
            evidence_text += (ctx.snapshot / test).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
    findings: list[Finding] = []
    if not evidence_paths and not re.search(
        r"(?i)(?:benchmark|runtime|latency|throughput|max_rss|memory|complexity)",
        evidence_text,
    ):
        findings.append(
            Finding(
                check_id="performance-evidence",
                rule_id="missing-performance-budget",
                severity=Severity.HIGH,
                message="hot-path change has no changed benchmark/budget or selected performance regression test",
                evidence={"hot_paths": [change.path for change in hot_changes]},
            )
        )
    for change in hot_changes:
        if "python" not in change.classes or "native" in change.classes:
            continue
        try:
            text = (ctx.snapshot / change.path).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        if "performance-critical" in text and not re.search(
            r"(?i)(?:numpy|torch\.|numba|triton|native|vectori[sz])", text
        ):
            findings.append(
                Finding(
                    check_id="performance-evidence",
                    rule_id="python-only-hotpath",
                    severity=Severity.HIGH,
                    path=change.path,
                    message="declared performance-critical path has no vectorized, compiled, or native execution route",
                )
            )
    return _result(
        "performance-evidence",
        started,
        findings,
        files=[change.path for change in hot_changes] + evidence_paths,
    )


def check_research_evidence(
    ctx: ReviewContext, selection: TestSelection
) -> CheckResult:
    started = time.perf_counter()
    novel_changes = [
        change
        for change in ctx.live_changes
        if "novel" in change.classes and "test" not in change.classes
    ]
    if not novel_changes:
        return _result("research-integrity", started)
    findings: list[Finding] = []
    test_text = ""
    for test in selection.tests:
        try:
            test_text += (ctx.snapshot / test).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
    changed_text = ""
    for change in novel_changes:
        try:
            changed_text += (ctx.snapshot / change.path).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
    combined = changed_text + "\n" + test_text
    if re.search(r"(?i)(?:promot|baseline|metric|score|receipt)", changed_text):
        missing = [
            token
            for token in ("baseline", "seed", "config", "fingerprint")
            if token not in combined.casefold()
        ]
        if missing:
            findings.append(
                Finding(
                    check_id="research-integrity",
                    rule_id="incomplete-result-provenance",
                    severity=Severity.HIGH,
                    message="research decision path lacks exact identity/provenance fields: "
                    + ", ".join(missing),
                    evidence={
                        "changed_paths": [change.path for change in novel_changes]
                    },
                )
            )
    if re.search(
        r"(?i)(?:dtype|device|numerical|stability|finite|nan|inf)", changed_text
    ):
        if not re.search(r"(?i)(?:dtype|device|isfinite|nan|raises|error)", test_text):
            findings.append(
                Finding(
                    check_id="research-integrity",
                    rule_id="missing-numerical-device-tests",
                    severity=Severity.HIGH,
                    message="numerical/device-sensitive research change lacks selected dtype/device/finiteness tests",
                )
            )
    return _result(
        "research-integrity",
        started,
        findings,
        files=[change.path for change in novel_changes] + list(selection.tests),
    )


def check_native_source(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    findings: list[Finding] = []
    files = _changed_files(ctx, {"native"})
    dangerous = re.compile(r"\b(?:gets|strcpy|strcat|sprintf|system|popen)\s*\(")
    for rel in files:
        try:
            text = (ctx.snapshot / rel).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for match in dangerous.finditer(text):
            findings.append(
                Finding(
                    check_id="native-source",
                    rule_id="unsafe-native-api",
                    severity=Severity.CRITICAL,
                    path=rel,
                    line=text.count("\n", 0, match.start()) + 1,
                    message=f"unsafe native API admitted: {match.group(0).strip()}",
                )
            )
    return _result("native-source", started, findings, files=files)


BUILTIN_CHECKS["native-source"] = check_native_source


def _function_body_digest(node: ast.FunctionDef | ast.AsyncFunctionDef) -> str | None:
    body = list(node.body)
    if (
        body
        and isinstance(body[0], ast.Expr)
        and isinstance(body[0].value, ast.Constant)
    ):
        if isinstance(body[0].value.value, str):
            body = body[1:]
    length = (node.end_lineno or node.lineno) - node.lineno + 1
    if length < 10 or not body:
        return None
    normalized = ast.dump(ast.Module(body=body, type_ignores=[]), annotate_fields=True)
    return hashlib.sha256(normalized.encode("utf-8")).hexdigest()


def check_duplicate_function_bodies(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    changed = [change.path for change in ctx.live_changes if "python" in change.classes]
    if not changed:
        return _result("duplicate-function-bodies", started)
    changed_lines = changed_line_numbers(ctx.repo, ctx.candidate, changed)
    bodies: dict[str, list[tuple[str, str, int]]] = {}
    changed_bodies: set[tuple[str, str, int]] = set()
    for path in ctx.snapshot.rglob("*.py"):
        if any(
            part in {".venv", "node_modules", "__pycache__", ".run"}
            for part in path.parts
        ):
            continue
        rel = path.relative_to(ctx.snapshot).as_posix()
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=rel)
        except (OSError, UnicodeDecodeError, SyntaxError):
            continue
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            digest = _function_body_digest(node)
            if digest is None:
                continue
            location = (rel, node.name, node.lineno)
            bodies.setdefault(digest, []).append(location)
            lines = changed_lines.get(rel, set())
            if lines and any(
                node.lineno <= line <= (node.end_lineno or node.lineno)
                for line in lines
            ):
                changed_bodies.add(location)
    findings: list[Finding] = []
    emitted: set[tuple[str, str, int]] = set()
    for locations in bodies.values():
        if len(locations) < 2:
            continue
        originals = sorted(locations)
        for location in originals:
            if location not in changed_bodies or location in emitted:
                continue
            peers = [peer for peer in originals if peer != location]
            findings.append(
                Finding(
                    check_id="duplicate-function-bodies",
                    rule_id="copied-function-body",
                    severity=Severity.HIGH,
                    path=location[0],
                    line=location[2],
                    message=(
                        f"changed function {location[1]} duplicates "
                        + ", ".join(
                            f"{path}:{line} ({name})" for path, name, line in peers[:5]
                        )
                    ),
                    evidence={"peers": peers},
                )
            )
            emitted.add(location)
    return _result(
        "duplicate-function-bodies",
        started,
        findings,
        files=changed,
        metrics={
            "candidate_function_bodies": sum(len(items) for items in bodies.values())
        },
    )


BUILTIN_CHECKS["duplicate-function-bodies"] = check_duplicate_function_bodies


def check_structure_audit(ctx: ReviewContext) -> CheckResult:
    """Delegate to quality_checks, which imports this module for its helpers."""

    from conductor.candidate_review.quality_checks import (
        check_structure_audit as audit_structure,
    )

    return audit_structure(ctx)


BUILTIN_CHECKS["structure-audit"] = check_structure_audit


def check_equivalence_probe(ctx: ReviewContext) -> CheckResult:
    """Tier-1 gate: no change ships with a reachable branch no test drives.

    The mutation gate asks whether a test notices a corrupted line. This asks what
    that cannot answer -- whether the changed code does anything, and whether the
    tests can tell. Only REACHABLE_BUT_UNTESTED is a finding: a construct that
    changes behaviour in a regime the tests never reach. Sampling cannot prove the
    converse, so "nothing moved" is reported by the CLI and never blocks here.

    It runs against the candidate snapshot, not the working tree, because that is
    the tree that ships.
    """
    started = time.perf_counter()
    from conductor import slop_gate, slop_ledger

    modules = [
        change.path
        for change in ctx.live_changes
        if change.path.endswith(".py")
        and "test" not in change.classes
        and not Path(change.path).name.startswith("test_")
    ]
    if not modules:
        return _result("equivalence-probe", started, (), files=())

    _, summary = slop_gate.run("HEAD", ctx.snapshot, only=modules)

    # The gate already paid for this measurement, so hand it to the backlog rather
    # than discarding it. A run ARTIFACT, not a ledger write: this check runs against
    # a candidate snapshot and concurrently with other reviews, and a review that
    # mutates a tracked file dirties the tree it is reviewing. `make slop-backlog`
    # folds these in, which is what keeps the backlog current between full sweeps.
    _record_for_backlog(summary, ctx)

    # Severity follows the tier, which is what makes this check enforceable: a
    # finding in shipped code blocks, one in a one-off research script reports. The
    # tier comes from slop_ledger.aggregate rather than a second copy of the prefix
    # list, so the gate and the backlog can never disagree about what ships.
    tiers = {
        (item["module"], item["qualname"]): item["tier"]
        for item in slop_ledger.aggregate(
            slop_ledger.findings_from_summary({"blocking": summary["blocking"]})
        )
    }
    findings = [
        Finding(
            check_id="equivalence-probe",
            rule_id=item["rule"],
            severity=(
                Severity.HIGH
                if tiers.get((item["module"], item["qualname"])) == "shipped"
                else Severity.LOW
            ),
            path=item["module"],
            line=item["lineno"],
            message=(
                f"{item['qualname']}: {item['description']} changes the result only "
                f"under {item.get('amplifier')} (relative change "
                f"{item.get('max_diff_amplified'):.3e}); no test drives that regime. "
                "Cover it or remove the construct."
            ),
            evidence={
                k: item[k] for k in ("qualname", "verdict", "amplifier") if k in item
            },
        )
        for item in summary["blocking"]
    ]
    return _result(
        "equivalence-probe",
        started,
        findings,
        files=modules,
        metrics={
            "modules_probed": summary["modules_probed"],
            "advisory": len(summary["advisory"]),
            "without_drivers": len(summary["modules_without_drivers"]),
        },
    )


def _record_for_backlog(summary: dict, ctx: ReviewContext) -> None:
    """Drop this run's summary where `make slop-backlog` will find it.

    Best effort by design: a full disk or a read-only checkout must not fail a code
    review over bookkeeping. Nothing downstream reads a partial write, because the
    file is renamed into place only once it is complete.
    """
    from conductor.slop_ledger import GATE_FINDINGS

    try:
        GATE_FINDINGS.mkdir(parents=True, exist_ok=True)
        stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%S%f")
        final = GATE_FINDINGS / f"gate-{stamp}.json"
        tmp = final.with_suffix(".json.part")
        tmp.write_text(json.dumps(summary, indent=2) + "\n")
        tmp.rename(final)
    except OSError:
        pass


BUILTIN_CHECKS["equivalence-probe"] = check_equivalence_probe


def files_for_policy(ctx: ReviewContext, check: CheckPolicy) -> list[str]:
    class_filter = set(check.classes)
    excluded = set(check.exclude_classes)
    files = [
        change.path
        for change in ctx.candidate.changes
        if (check.run_on_deletions or not change.deleted)
        and (not class_filter or class_filter.intersection(change.classes))
        and not excluded.intersection(change.classes)
    ]
    return sorted(set(files))


def run_builtin(ctx: ReviewContext, check: CheckPolicy) -> CheckResult:
    runner = BUILTIN_CHECKS.get(check.check_id)
    if runner is None:
        started = time.perf_counter()
        return _result(
            check.check_id,
            started,
            [
                Finding(
                    check_id=check.check_id,
                    rule_id="unknown-builtin",
                    severity=Severity.CRITICAL,
                    message=f"policy names an unavailable built-in check: {check.check_id}",
                )
            ],
        )
    return runner(ctx)
