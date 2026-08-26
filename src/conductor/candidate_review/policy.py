"""Strict policy loading, path classification, baselines, and exceptions."""

from __future__ import annotations

import fnmatch
import re
import tomllib
from dataclasses import dataclass
from datetime import date, datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, Iterable

from conductor.candidate_review.model import (
    Change,
    Finding,
    Severity,
    sha256_bytes,
    sha256_file,
)

ALLOWED_TOP_LEVEL = {
    "schema_version",
    "block_at",
    "max_workers",
    "cache_ttl_days",
    "claim_max_age_hours",
    "max_file_bytes",
    "max_binary_bytes",
    "coverage_threshold",
    "high_risk_coverage_threshold",
    "baseline_expires",
    "classes",
    "risk",
    "paths",
    "checks",
    "baselines",
    "exceptions",
}
ALLOWED_CHECK_KEYS = {
    "kind",
    "profiles",
    "classes",
    "exclude_classes",
    "command",
    "version_command",
    "severity",
    "timeout_seconds",
    "memory_mb",
    "always",
    "cache",
    "run_on_deletions",
    "max_output_chars",
}
ALLOWED_EXCEPTION_KEYS = {
    "id",
    "check",
    "rule",
    "path",
    "fingerprint",
    "owner",
    "justification",
    "expires",
}
VALID_CLASSES = {
    "binary",
    "config",
    "dependency",
    "docs",
    "generated",
    "governance",
    "native",
    "notebook",
    "novel",
    "node_dependency",
    "python",
    "python_dependency",
    "research_result",
    "rust_dependency",
    "shell",
    "source",
    "symlink",
    "test",
    "toml",
    "workflow",
    "web",
}


class PolicyError(RuntimeError):
    """The candidate policy is absent, malformed, broad, or stale."""


@dataclass(frozen=True, slots=True)
class CheckPolicy:
    check_id: str
    kind: str
    profiles: tuple[str, ...]
    classes: tuple[str, ...]
    exclude_classes: tuple[str, ...]
    command: tuple[str, ...]
    version_command: tuple[str, ...]
    severity: Severity
    timeout_seconds: int
    memory_mb: int
    always: bool
    cache: bool
    run_on_deletions: bool
    max_output_chars: int


@dataclass(frozen=True, slots=True)
class BaselinePolicy:
    baseline_id: str
    path: str
    classes: tuple[str, ...]
    required_profiles: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class ExceptionPolicy:
    exception_id: str
    check_id: str
    rule_id: str | None
    path: str
    fingerprint: str | None
    owner: str
    justification: str
    expires: date

    def matches(self, finding: Finding) -> bool:
        if self.check_id != finding.check_id:
            return False
        if self.rule_id and self.rule_id != finding.rule_id:
            return False
        if self.fingerprint and self.fingerprint != finding.fingerprint:
            return False
        return finding.path is not None and fnmatch.fnmatchcase(finding.path, self.path)


@dataclass(frozen=True, slots=True)
class Policy:
    path: Path
    digest: str
    schema_version: int
    block_at: Severity
    max_workers: int
    cache_ttl_days: int
    claim_max_age_hours: int
    max_file_bytes: int
    max_binary_bytes: int
    coverage_threshold: float
    high_risk_coverage_threshold: float
    baseline_expires: date
    class_globs: dict[str, tuple[str, ...]]
    high_risk_globs: tuple[str, ...]
    protected_delete_globs: tuple[str, ...]
    hot_path_globs: tuple[str, ...]
    generated_globs: tuple[str, ...]
    checks: tuple[CheckPolicy, ...]
    baselines: tuple[BaselinePolicy, ...]
    exceptions: tuple[ExceptionPolicy, ...]

    def classify_change(self, change: Change) -> Change:
        candidate_paths = tuple(
            path for path in (change.path, change.old_path) if path is not None
        )
        classes = set(_intrinsic_classes(change))
        if change.old_path:
            classes.update(
                _intrinsic_classes(
                    change,
                    path_override=change.old_path,
                    mode_override=change.old_mode,
                )
            )
        for class_name, patterns in self.class_globs.items():
            if any(_matches_any(path, patterns) for path in candidate_paths):
                classes.add(class_name)
        if any(_matches_any(path, self.generated_globs) for path in candidate_paths):
            classes.add("generated")
        risk = (
            "high"
            if any(_matches_any(path, self.high_risk_globs) for path in candidate_paths)
            else "normal"
        )
        return Change(
            status=change.status,
            path=change.path,
            old_path=change.old_path,
            old_mode=change.old_mode,
            new_mode=change.new_mode,
            old_oid=change.old_oid,
            new_oid=change.new_oid,
            classes=tuple(sorted(classes)),
            risk=risk,
        )

    def active_checks(self, profile: str) -> tuple[CheckPolicy, ...]:
        return tuple(check for check in self.checks if profile in check.profiles)


def _matches_any(path: str, patterns: Iterable[str]) -> bool:
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def _intrinsic_classes(
    change: Change,
    *,
    path_override: str | None = None,
    mode_override: str | None = None,
) -> set[str]:
    path = PurePosixPath(path_override or change.path)
    suffix = path.suffix.lower()
    classes: set[str] = set()
    if suffix in {".py", ".pyi"}:
        classes.update({"python", "source"})
    elif suffix in {".c", ".cc", ".cpp", ".cxx", ".cu", ".cuh", ".h", ".hpp", ".rs"}:
        classes.update({"native", "source"})
    elif suffix in {".js", ".jsx", ".ts", ".tsx", ".css"}:
        classes.update({"source", "web"})
    elif suffix in {".sh", ".bash"}:
        classes.update({"source", "shell"})
    elif suffix in {".md", ".rst", ".txt"}:
        classes.add("docs")
    elif suffix == ".ipynb":
        classes.add("notebook")
    elif suffix in {".toml", ".yaml", ".yml", ".json", ".ini", ".cfg"}:
        classes.add("config")
        if suffix == ".toml":
            classes.add("toml")
    if suffix in {".so", ".dll", ".dylib", ".a", ".o", ".pt", ".pth", ".bin"}:
        classes.add("binary")
    if path.name in {
        "pyproject.toml",
        "uv.lock",
        "requirements.txt",
        "requirements-dev.txt",
    }:
        classes.update({"dependency", "python_dependency"})
    if path.name in {"package.json", "package-lock.json", "npm-shrinkwrap.json"}:
        classes.update({"dependency", "node_dependency"})
    if path.name in {"Cargo.toml", "Cargo.lock"}:
        classes.update({"dependency", "rust_dependency"})
    name = path.name
    if (
        "test" in path.parts
        or name.startswith("test_")
        or name.endswith(
            (
                "_test.py",
                "_test.c",
                "_test.cc",
                "_test.cpp",
                "_test.cxx",
                ".test.js",
                ".test.jsx",
                ".test.ts",
                ".test.tsx",
                ".spec.js",
                ".spec.jsx",
                ".spec.ts",
                ".spec.tsx",
                "Test.java",
            )
        )
    ):
        classes.add("test")
    if (mode_override or change.new_mode) == "120000":
        classes.add("symlink")
    return classes


def _string_tuple(
    value: Any, *, field: str, allow_empty: bool = True
) -> tuple[str, ...]:
    if not isinstance(value, list) or any(
        not isinstance(item, str) or not item for item in value
    ):
        raise PolicyError(f"{field} must be an array of non-empty strings")
    if not allow_empty and not value:
        raise PolicyError(f"{field} must not be empty")
    return tuple(value)


def _positive_int(value: Any, *, field: str, maximum: int | None = None) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        raise PolicyError(f"{field} must be a positive integer")
    if maximum is not None and value > maximum:
        raise PolicyError(f"{field} must be <= {maximum}, got {value}")
    return value


def _float_percent(value: Any, *, field: str) -> float:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        raise PolicyError(f"{field} must be numeric")
    result = float(value)
    if result < 0.0 or result > 100.0:
        raise PolicyError(f"{field} must be between 0 and 100")
    return result


def _bool_value(value: Any, *, field: str) -> bool:
    if not isinstance(value, bool):
        raise PolicyError(f"{field} must be a boolean")
    return value


def _date_value(value: Any, *, field: str) -> date:
    if isinstance(value, datetime):
        return value.date()
    if isinstance(value, date):
        return value
    if isinstance(value, str):
        try:
            return date.fromisoformat(value)
        except ValueError as exc:
            raise PolicyError(f"{field} must be an ISO date") from exc
    raise PolicyError(f"{field} must be an ISO date")


def _parse_check(check_id: str, raw: Any) -> CheckPolicy:
    if not isinstance(raw, dict):
        raise PolicyError(f"checks.{check_id} must be a table")
    unknown = set(raw) - ALLOWED_CHECK_KEYS
    if unknown:
        raise PolicyError(f"checks.{check_id} has unknown keys: {sorted(unknown)}")
    kind = raw.get("kind")
    if kind not in {"builtin", "command"}:
        raise PolicyError(f"checks.{check_id}.kind must be builtin or command")
    profiles = _string_tuple(
        raw.get("profiles"), field=f"checks.{check_id}.profiles", allow_empty=False
    )
    if set(profiles) - {"fast", "full"}:
        raise PolicyError(f"checks.{check_id}.profiles contains an unknown profile")
    classes = _string_tuple(raw.get("classes", []), field=f"checks.{check_id}.classes")
    if set(classes) - VALID_CLASSES:
        raise PolicyError(f"checks.{check_id}.classes contains an unknown class")
    exclude_classes = _string_tuple(
        raw.get("exclude_classes", []), field=f"checks.{check_id}.exclude_classes"
    )
    if set(exclude_classes) - VALID_CLASSES:
        raise PolicyError(
            f"checks.{check_id}.exclude_classes contains an unknown class"
        )
    command = _string_tuple(raw.get("command", []), field=f"checks.{check_id}.command")
    version = _string_tuple(
        raw.get("version_command", []), field=f"checks.{check_id}.version_command"
    )
    if kind == "command" and (not command or not version):
        raise PolicyError(
            f"command check {check_id} requires command and version_command"
        )
    try:
        severity = Severity(str(raw.get("severity", "high")))
    except ValueError as exc:
        raise PolicyError(f"checks.{check_id}.severity is invalid") from exc
    return CheckPolicy(
        check_id=check_id,
        kind=kind,
        profiles=profiles,
        classes=classes,
        exclude_classes=exclude_classes,
        command=command,
        version_command=version,
        severity=severity,
        timeout_seconds=_positive_int(
            raw.get("timeout_seconds", 60),
            field=f"checks.{check_id}.timeout_seconds",
            maximum=3600,
        ),
        memory_mb=_positive_int(
            raw.get("memory_mb", 2048),
            field=f"checks.{check_id}.memory_mb",
            maximum=65536,
        ),
        always=_bool_value(raw.get("always", False), field=f"checks.{check_id}.always"),
        cache=_bool_value(raw.get("cache", True), field=f"checks.{check_id}.cache"),
        run_on_deletions=_bool_value(
            raw.get("run_on_deletions", False),
            field=f"checks.{check_id}.run_on_deletions",
        ),
        max_output_chars=_positive_int(
            raw.get("max_output_chars", 12000),
            field=f"checks.{check_id}.max_output_chars",
        ),
    )


def _validate_exception_path(path: str) -> None:
    if path in {"*", "**", "**/*", ".", "./*"} or path.startswith("/"):
        raise PolicyError(f"exception path is a forbidden blanket scope: {path!r}")
    literals = [
        part for part in PurePosixPath(path).parts if not re.search(r"[*?\[]", part)
    ]
    if len(literals) < 2:
        raise PolicyError(
            f"exception path must have at least two literal segments: {path!r}"
        )


def _parse_exception(raw: Any) -> ExceptionPolicy:
    if not isinstance(raw, dict):
        raise PolicyError("each exceptions entry must be a table")
    unknown = set(raw) - ALLOWED_EXCEPTION_KEYS
    if unknown:
        raise PolicyError(f"exception has unknown keys: {sorted(unknown)}")
    required = {"id", "check", "path", "owner", "justification", "expires"}
    missing = required - set(raw)
    if missing:
        raise PolicyError(f"exception is missing required keys: {sorted(missing)}")
    path = str(raw["path"])
    _validate_exception_path(path)
    justification = str(raw["justification"]).strip()
    owner = str(raw["owner"]).strip()
    if len(justification) < 20 or len(owner) < 2:
        raise PolicyError("exception owner/justification is not specific enough")
    return ExceptionPolicy(
        exception_id=str(raw["id"]),
        check_id=str(raw["check"]),
        rule_id=str(raw["rule"]) if raw.get("rule") else None,
        path=path,
        fingerprint=str(raw["fingerprint"]) if raw.get("fingerprint") else None,
        owner=owner,
        justification=justification,
        expires=_date_value(raw["expires"], field="exceptions.expires"),
    )


def _parse_baselines(raw: Any) -> tuple[BaselinePolicy, ...]:
    if not isinstance(raw, dict):
        raise PolicyError("baselines must be a table")
    baselines: list[BaselinePolicy] = []
    for baseline_id, value in raw.items():
        if not isinstance(value, dict) or set(value) != {
            "path",
            "classes",
            "required_profiles",
        }:
            raise PolicyError(f"baselines.{baseline_id} has an invalid schema")
        baselines.append(
            BaselinePolicy(
                baseline_id=baseline_id,
                path=str(value["path"]),
                classes=_string_tuple(
                    value["classes"], field=f"baselines.{baseline_id}.classes"
                ),
                required_profiles=_string_tuple(
                    value["required_profiles"],
                    field=f"baselines.{baseline_id}.required_profiles",
                ),
            )
        )
    return tuple(baselines)


def load_policy(path: Path) -> Policy:
    try:
        raw_bytes = path.read_bytes()
    except OSError as exc:
        raise PolicyError(
            f"required candidate policy is unreadable: {path}: {exc}"
        ) from exc
    try:
        raw = tomllib.loads(raw_bytes.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
        raise PolicyError(f"malformed candidate policy {path}: {exc}") from exc
    unknown = set(raw) - ALLOWED_TOP_LEVEL
    if unknown:
        raise PolicyError(
            f"candidate policy has unknown top-level keys: {sorted(unknown)}"
        )
    if raw.get("schema_version") != 1:
        raise PolicyError(
            f"unsupported policy schema_version: {raw.get('schema_version')!r}"
        )
    class_raw = raw.get("classes")
    if not isinstance(class_raw, dict):
        raise PolicyError("classes must be a table")
    unknown_classes = set(class_raw) - VALID_CLASSES
    if unknown_classes:
        raise PolicyError(f"classes contains unknown names: {sorted(unknown_classes)}")
    class_globs = {
        name: _string_tuple(value, field=f"classes.{name}")
        for name, value in class_raw.items()
    }
    risk_raw = raw.get("risk")
    paths_raw = raw.get("paths")
    checks_raw = raw.get("checks")
    baselines_raw = raw.get("baselines", {})
    if not isinstance(risk_raw, dict) or set(risk_raw) != {"high"}:
        raise PolicyError("risk must contain exactly the high array")
    if not isinstance(paths_raw, dict) or set(paths_raw) != {
        "protected_deletes",
        "hot",
        "generated",
    }:
        raise PolicyError(
            "paths must contain exactly protected_deletes, hot, and generated"
        )
    if not isinstance(checks_raw, dict) or not checks_raw:
        raise PolicyError("checks must be a non-empty table")
    exceptions_raw = raw.get("exceptions", [])
    if not isinstance(exceptions_raw, list):
        raise PolicyError("exceptions must be an array of tables")
    try:
        block_at = Severity(str(raw["block_at"]))
    except (KeyError, ValueError) as exc:
        raise PolicyError("block_at must be a valid severity") from exc
    policy = Policy(
        path=path,
        digest=sha256_bytes(raw_bytes),
        schema_version=1,
        block_at=block_at,
        max_workers=_positive_int(
            raw.get("max_workers"), field="max_workers", maximum=16
        ),
        cache_ttl_days=_positive_int(
            raw.get("cache_ttl_days"), field="cache_ttl_days", maximum=365
        ),
        claim_max_age_hours=_positive_int(
            raw.get("claim_max_age_hours"), field="claim_max_age_hours", maximum=720
        ),
        max_file_bytes=_positive_int(raw.get("max_file_bytes"), field="max_file_bytes"),
        max_binary_bytes=_positive_int(
            raw.get("max_binary_bytes"), field="max_binary_bytes"
        ),
        coverage_threshold=_float_percent(
            raw.get("coverage_threshold"), field="coverage_threshold"
        ),
        high_risk_coverage_threshold=_float_percent(
            raw.get("high_risk_coverage_threshold"),
            field="high_risk_coverage_threshold",
        ),
        baseline_expires=_date_value(
            raw.get("baseline_expires"), field="baseline_expires"
        ),
        class_globs=class_globs,
        high_risk_globs=_string_tuple(risk_raw["high"], field="risk.high"),
        protected_delete_globs=_string_tuple(
            paths_raw["protected_deletes"], field="paths.protected_deletes"
        ),
        hot_path_globs=_string_tuple(paths_raw["hot"], field="paths.hot"),
        generated_globs=_string_tuple(paths_raw["generated"], field="paths.generated"),
        checks=tuple(
            _parse_check(check_id, value) for check_id, value in checks_raw.items()
        ),
        baselines=_parse_baselines(baselines_raw),
        exceptions=tuple(_parse_exception(value) for value in exceptions_raw),
    )
    _validate_policy(policy)
    return policy


def _validate_policy(policy: Policy) -> None:
    today = datetime.now(timezone.utc).date()
    if policy.baseline_expires < today:
        raise PolicyError(
            f"policy baseline window expired on {policy.baseline_expires}; refresh and re-review it"
        )
    identifiers = [exception.exception_id for exception in policy.exceptions]
    if len(identifiers) != len(set(identifiers)):
        raise PolicyError("exception identifiers must be unique")
    known_checks = {check.check_id for check in policy.checks}
    for exception in policy.exceptions:
        if exception.check_id not in known_checks:
            raise PolicyError(
                f"exception {exception.exception_id} names an unknown check"
            )
        if exception.expires < today:
            raise PolicyError(
                f"exception {exception.exception_id} expired on {exception.expires}"
            )
        if (exception.expires - today).days > 90:
            raise PolicyError(
                f"exception {exception.exception_id} expires more than 90 days out"
            )


def baseline_receipts(
    policy: Policy, snapshot: Path, profile: str, classes: set[str]
) -> list[dict[str, Any]]:
    receipts: list[dict[str, Any]] = []
    for baseline in policy.baselines:
        if profile not in baseline.required_profiles or not classes.intersection(
            baseline.classes
        ):
            continue
        path = snapshot / baseline.path
        if not path.is_file():
            raise PolicyError(
                f"required baseline is absent from candidate tree: {baseline.path}"
            )
        receipts.append(
            {
                "id": baseline.baseline_id,
                "path": baseline.path,
                "sha256": sha256_file(path),
                "expires": policy.baseline_expires.isoformat(),
            }
        )
    return receipts


def apply_exceptions(policy: Policy, findings: list[Finding]) -> list[Finding]:
    for finding in findings:
        finding.finalize()
        matches = [
            exception for exception in policy.exceptions if exception.matches(finding)
        ]
        if len(matches) > 1:
            raise PolicyError(
                f"finding {finding.fingerprint} matches multiple exceptions: "
                + ", ".join(match.exception_id for match in matches)
            )
        if matches:
            finding.exception_id = matches[0].exception_id
    return findings
