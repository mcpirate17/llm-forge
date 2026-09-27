"""Built-in policy checks, graph-selected tests, and hermetic command execution."""

from __future__ import annotations

import ast
import fnmatch
import json
import os
import time
import tomllib
from collections.abc import Callable, Iterable, Sequence
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path

from conductor._native import duplicate_body_fingerprints_native
from conductor.candidate_review.equivalence_probe_check import (
    check_equivalence_probe,
)
from conductor.candidate_review.git_source import changed_line_numbers
from conductor.candidate_review.import_declaration import check_import_declaration
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
from conductor.candidate_review.scan_ledger import ScanLedger


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


def _native_eval(operation: str, payload: dict[str, object]) -> dict[str, object]:
    from conductor._native import candidate_checks_native

    return json.loads(candidate_checks_native(operation, json.dumps(payload)))


def _native_findings(payload: dict[str, object]) -> list[Finding]:
    return [
        Finding(**{**item, "severity": Severity(item["severity"])})
        for item in payload["findings"]
    ]


def _native_entries(entries: Sequence[TreeEntry]) -> list[dict[str, str]]:
    return [
        {
            "path": entry.path,
            "mode": entry.mode,
            "folded": entry.path.casefold(),
            "repr": repr(entry.path),
        }
        for entry in entries
    ]


def _tree_integrity_findings(
    ctx: ReviewContext,
) -> tuple[list[Finding], dict[str, TreeEntry]]:
    native = _native_eval("tree-integrity", {"entries": _native_entries(ctx.entries)})
    return _native_findings(native), {entry.path: entry for entry in ctx.entries}


def _native_integrity_changes(ctx: ReviewContext) -> list[dict[str, object]]:
    entries = {entry.path: entry for entry in ctx.entries}
    changes: list[dict[str, object]] = []
    for change in ctx.candidate.changes:
        row: dict[str, object] = {**asdict(change), "deleted": change.deleted}
        entry = entries.get(change.path)
        if not change.deleted and entry is not None:
            path = ctx.snapshot / change.path
            row["exists"] = os.path.lexists(path)
            if row["exists"]:
                if entry.mode == "120000":
                    row["target"] = os.readlink(path)
                else:
                    row["size"] = path.stat().st_size
        changes.append(row)
    return changes


def check_candidate_integrity(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    native = _native_eval(
        "candidate-integrity",
        {
            "entries": _native_entries(ctx.entries),
            "changes": _native_integrity_changes(ctx),
            "surface": ctx.surface,
            "protected_globs": ctx.policy.protected_delete_globs,
            "max_file_bytes": ctx.policy.max_file_bytes,
            "max_binary_bytes": ctx.policy.max_binary_bytes,
        },
    )
    return _result(
        "candidate-integrity",
        started,
        _native_findings(native),
        files=[change.path for change in ctx.candidate.changes],
        metrics=native["metrics"],
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
        except Exception as exc:  # noqa: BLE001 - normalize third-party parser failures
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
        raise ValueError(  # noqa: TRY004 - all notebook parse failures share one path
            "notebook must be an object with a cells array"
        )
    if payload.get("nbformat") != 4:
        raise ValueError(f"unsupported notebook format: {payload.get('nbformat')!r}")
    for index, cell in enumerate(payload["cells"]):
        if not isinstance(cell, dict):
            raise ValueError(  # noqa: TRY004 - all notebook parse failures share one path
                f"cell {index} is not an object"
            )
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
    files: list[dict[str, str]] = []
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
        files.append({"path": change.path, "text": text})
    native = _native_eval("secret-scan", {"files": files})
    return _result(
        "secret-scan",
        started,
        _native_findings(native),
        files=[item["path"] for item in files],
    )


class _PythonVisitor(ast.NodeVisitor):
    """Compatibility seam for direct visitor callers; rules live in Rust."""

    def __init__(self, rel: str, lines: list[str], hot_path: bool) -> None:
        self.rel = rel
        self.lines = lines
        self.hot_path = hot_path
        self.findings: list[Finding] = []

    def visit(self, node: ast.AST) -> None:
        if not isinstance(node, ast.Module):
            raise TypeError("candidate Python visitor requires a module AST")
        native = _native_eval(
            "python-ast",
            {
                "path": self.rel,
                "text": "\n".join(self.lines),
                "lines": self.lines,
                "changed_lines": [],
                "classes": [],
                "hot": self.hot_path,
            },
        )
        self.findings.extend(_native_findings(native))


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
            ast.parse(text, filename=rel)
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
        native = _native_eval(
            "python-ast",
            {
                "path": rel,
                "text": text,
                "lines": text.splitlines(),
                "changed_lines": sorted(line_map.get(rel, set())),
                "classes": changes[rel].classes,
                "hot": any(
                    fnmatch.fnmatchcase(rel, pattern)
                    for pattern in ctx.policy.hot_path_globs
                ),
            },
        )
        findings.extend(_native_findings(native))
    return _result("python-ast", started, findings, files=files)


def check_dependency_integrity(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    paths = {
        change.path
        for change in ctx.candidate.changes
        if "dependency" in change.classes
    }
    native = _native_eval(
        "dependency-integrity", {"paths": list(paths), "snapshot": str(ctx.snapshot)}
    )
    return _result(
        "dependency-integrity",
        started,
        _native_findings(native),
        files=paths,
        metrics=native["metrics"],
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
    now = datetime.now(UTC)
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
    "import-declaration": check_import_declaration,
    "mutation-evidence": check_mutation_evidence,
    "ownership": check_ownership,
    "python-ast": check_python_ast,
    "secret-scan": check_secrets,
}


def check_performance_evidence(
    ctx: ReviewContext, selection: TestSelection
) -> CheckResult:
    started = time.perf_counter()
    chosen = _native_eval(
        "performance-selection",
        {
            "changes": [asdict(change) for change in ctx.live_changes],
            "hot_globs": ctx.policy.hot_path_globs,
        },
    )
    hot_paths = chosen["hot_paths"]
    if not hot_paths:
        return _result("performance-evidence", started)
    evidence_paths = chosen["evidence_paths"]
    hot_changes = [change for change in ctx.live_changes if change.path in hot_paths]
    evidence_text = ""
    for test in selection.tests:
        try:
            evidence_text += (ctx.snapshot / test).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
    rows: list[dict[str, object]] = []
    for change in hot_changes:
        row: dict[str, object] = {"path": change.path, "classes": change.classes}
        if "python" in change.classes and "native" not in change.classes:
            try:
                row["text"] = (ctx.snapshot / change.path).read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                pass
        rows.append(row)
    native = _native_eval(
        "performance-evidence",
        {"hot_changes": rows, "evidence_paths": evidence_paths, "evidence_text": evidence_text},
    )
    return _result(
        "performance-evidence",
        started,
        _native_findings(native),
        files=hot_paths + evidence_paths,
    )


def _research_texts(
    ctx: ReviewContext, novel_changes: Sequence[Change]
) -> tuple[str, str, str]:
    """``(trigger_text, evidence_text, diff_error)`` for the novel changes.

    Two different questions read two different texts. *What did this candidate
    do?* is answered by the lines it added or altered, so that is what arms the
    rules below. *What evidence stands behind it?* may have been recorded by an
    earlier change, so that still reads each changed file whole.

    Arming on whole files charged a change for words it never wrote: any edit to
    a file that merely mentioned a device or a NaN demanded dtype/device tests,
    and a documentation-only change selects no tests at all, so no edit to such
    a file could pass. Fail-closed is preserved -- if the diff cannot be read,
    every changed file arms the rules exactly as before.
    """

    evidence_parts: list[str] = []
    sources: dict[str, str] = {}
    for change in novel_changes:
        try:
            text = (ctx.snapshot / change.path).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        sources[change.path] = text
        evidence_parts.append(text)
    evidence_text = "\n".join(evidence_parts)
    try:
        changed_lines = changed_line_numbers(ctx.repo, ctx.candidate, sorted(sources))
    except (RuntimeError, ValueError, OSError) as exc:
        # Not swallowed: the caller reports the reason in the check metrics, and
        # the whole-file text it falls back to is the stricter of the two.
        return evidence_text, evidence_text, f"{type(exc).__name__}: {exc}"
    trigger_parts: list[str] = []
    for path, text in sources.items():
        lines = text.splitlines()
        trigger_parts.extend(
            lines[number - 1]
            for number in sorted(changed_lines.get(path, set()))
            if 1 <= number <= len(lines)
        )
    return "\n".join(trigger_parts), evidence_text, ""


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
    test_text = ""
    for test in selection.tests:
        try:
            test_text += (ctx.snapshot / test).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
    changed_text, evidence_text, diff_error = _research_texts(ctx, novel_changes)
    combined = evidence_text + "\n" + test_text
    native = _native_eval(
        "research-integrity",
        {
            "changed_text": changed_text,
            "combined_casefold": combined.casefold(),
            "test_text": test_text,
            "changed_paths": [change.path for change in novel_changes],
        },
    )
    return _result(
        "research-integrity",
        started,
        _native_findings(native),
        files=[change.path for change in novel_changes] + list(selection.tests),
        metrics={
            "armed_on": "whole-files" if diff_error else "changed-lines",
            "diff_error": diff_error,
        },
    )


def check_native_source(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    files = _changed_files(ctx, {"native"})
    # Every file this check reads is one the candidate changed, so an unreadable
    # one is not a gap in some background corpus -- it is the check declaring a
    # PASS over native source nobody scanned for unsafe APIs.
    ledger = ScanLedger("native-source")
    records: list[dict[str, str]] = []
    for rel in files:
        if (text := ledger.read_text(ctx.snapshot, rel)) is not None:
            records.append({"path": rel, "text": text})
    native = _native_eval("native-source", {"files": records})
    findings = _native_findings(native)
    findings.extend(
        ledger.incomplete_findings(
            adjudicated=set(files),
            severity=Severity.CRITICAL,
            subject="changed native source",
        )
    )
    return _result(
        "native-source", started, findings, files=files, metrics=ledger.metrics()
    )


BUILTIN_CHECKS["native-source"] = check_native_source


def check_crate_version(ctx: ReviewContext) -> CheckResult:
    """Delegate to crate_version, which imports this module for its helpers."""

    from conductor.candidate_review.crate_version import (
        check_crate_version as _run,
    )

    return _run(ctx)


BUILTIN_CHECKS["crate-version"] = check_crate_version


def check_duplicate_function_bodies(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    changed = [change.path for change in ctx.live_changes if "python" in change.classes]
    if not changed:
        return _result("duplicate-function-bodies", started)
    changed_lines = changed_line_numbers(ctx.repo, ctx.candidate, changed)
    bodies: dict[str, list[tuple[str, str, int]]] = {}
    changed_bodies: set[tuple[str, str, int]] = set()
    records: list[tuple[str, str]] = []
    ledger = ScanLedger("duplicate-function-bodies")
    for path in ctx.snapshot.rglob("*.py"):
        if any(
            part in {".venv", "node_modules", "__pycache__", ".run"}
            for part in path.parts
        ):
            continue
        rel = path.relative_to(ctx.snapshot).as_posix()
        source = ledger.read_text(ctx.snapshot, rel)
        if source is None:
            continue
        records.append((rel, source))
    native_files = json.loads(duplicate_body_fingerprints_native(records, "candidate"))
    for native_file in native_files:
        rel = native_file["path"]
        for node in native_file["functions"]:
            digest = node["digest"]
            location = (rel, node["name"], node["lineno"])
            bodies.setdefault(digest, []).append(location)
            lines = changed_lines.get(rel, set())
            if lines and any(
                node["lineno"] <= line <= node["end_lineno"] for line in lines
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
    findings.extend(
        ledger.incomplete_findings(
            adjudicated=set(changed),
            severity=Severity.HIGH,
            subject="candidate source",
        )
    )
    return _result(
        "duplicate-function-bodies",
        started,
        findings,
        files=changed,
        metrics={
            "candidate_function_bodies": sum(len(items) for items in bodies.values()),
            **ledger.metrics(),
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
BUILTIN_CHECKS["equivalence-probe"] = check_equivalence_probe


def files_for_policy(ctx: ReviewContext, check: CheckPolicy) -> list[str]:
    native = _native_eval(
        "files-for-policy",
        {
            "changes": [asdict(change) for change in ctx.candidate.changes],
            "classes": check.classes,
            "exclude_classes": check.exclude_classes,
            "run_on_deletions": check.run_on_deletions,
        },
    )
    return native["files"]


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
