"""Strict policy loading, path classification, baselines, and exceptions."""

from __future__ import annotations

import fnmatch
import json
import re
import tomllib
from collections.abc import Collection, Mapping, Sequence
from dataclasses import asdict, dataclass
from datetime import UTC, date, datetime
from pathlib import Path
from typing import Any

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
    "mutation_waivers",
    "value_waivers",
    "tools",
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
    "wall_timeout_seconds",
    "memory_mb",
    "always",
    "cache",
    "run_on_deletions",
    "max_output_chars",
    "attribution",
    "shard_max_files",
    "shard_workers",
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
# Moved 2026-08-29 from 58da5608 to d3697f22. Waivers activate only when the
# candidate's base commit equals this value, so the constant and the 100
# [[mutation_waivers]] must move together -- which is the point: relocating the
# integration point is a reviewed code change, not a data edit that quietly
# re-activates a hundred waivers. #48 through #52 landed on w7 after the waivers
# were authored, so their original base is no longer any candidate's base and
# every waiver was inactive. The per-file sha256 and pinned-source bindings are
# untouched and still enforced.
MUTATION_WAIVER_INTEGRATION_BASE = "d3697f22c2cb974dbae2d2dc4847c99d0de92224"
MUTATION_WAIVER_SOURCE_ANCHOR = "61343f575215dd222a74fc2c060d0328692ded5e"
W7_TRIDENT_LINEAR_INTEGRATION_MILESTONE = "w7-trident-linear-integration"
MUTATION_WAIVER_BINDING_CLAUSE = (
    "Any future edit to this test file or any pinned source file, or revival "
    "of its lane, voids this waiver and requires a mutation campaign before "
    "renewal."
)
WAIVER_SHA256_PATTERN = re.compile(r"\A[0-9a-f]{64}\Z")
TEST_NAME_SUFFIXES = (
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

ALLOWED_WAIVER_KEYS = {
    "id",
    "path",
    "owner",
    "justification",
    "expires",
    "milestone",
    "integration_base",
    "source_anchor",
    "sha256",
    "binding_clause",
    "sources",
}
VALID_CLASSES = {
    "binary",
    "cfamily",
    "cfamily_host",
    "config",
    "cuda",
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
    "rust",
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
    # "candidate": every finding blocks (default, fail-closed) -- the check judges
    # the candidate as a whole. "diff": a finding blocks only when it names a path
    # the candidate changed; anything else this check reports is pre-existing tree
    # debt, recorded and counted but not a wall in front of unrelated work.
    attribution: str = "candidate"
    shard_max_files: int = 0
    shard_workers: int = 1
    # 0 means "same as timeout_seconds"; read via `wall_timeout_seconds`.
    wall_timeout_override: int = 0

    @property
    def wall_timeout_seconds(self) -> int:
        """Wall budget for one subprocess, independent of its CPU budget.

        `timeout_seconds` bounds CPU via prlimit AND wall via subprocess timeout.
        Those measure different failures: a looping test burns CPU, while a shard
        sharing 4 vCPU with 3 siblings is slow in wall time having done nothing
        wrong. Raising this does NOT relax hang detection -- the CPU limit is
        untouched and still fires on a runaway.
        """
        return self.wall_timeout_override or self.timeout_seconds


@dataclass(frozen=True, slots=True)
class ToolPolicy:
    """One external binary the gate depends on, declared so it can never degrade.

    `expected_version` is CI's pin. A local version that differs is reported, not
    refused -- a version skew is worth knowing about but is not the failure that
    cost five days; a *missing* tool is.
    """

    tool_id: str
    executable: str
    version_command: tuple[str, ...]
    expected_version: str
    required_profiles: tuple[str, ...]
    provided_by: str
    rationale: str


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
        if self.fingerprint:
            return self.fingerprint == finding.fingerprint
        return finding.path is not None and fnmatch.fnmatchcase(finding.path, self.path)


@dataclass(frozen=True, slots=True)
class WaiverSourceBinding:
    """One exact pinned source dependency of a waived legacy test lane."""

    path: str
    sha256: str


@dataclass(frozen=True, slots=True)
class MutationWaiverPolicy:
    waiver_id: str
    path: str
    owner: str
    justification: str
    expires: date
    milestone: str
    integration_base: str
    source_anchor: str
    sha256: str
    binding_clause: str
    sources: tuple[WaiverSourceBinding, ...] = ()


@dataclass(frozen=True, slots=True)
class ValueWaiverPolicy:
    """A nodeid-exact exemption from new-test value gating, bound to one base."""

    integration_base: str
    nodeids: tuple[str, ...]
    reason: str
    approved_by: str
    approved_on: date
    expires: date | None


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
    mutation_waivers: tuple[MutationWaiverPolicy, ...] = ()
    value_waivers: tuple[ValueWaiverPolicy, ...] = ()
    tools: tuple[ToolPolicy, ...] = ()
    expired_exceptions: tuple[ExceptionPolicy, ...] = ()
    expired_mutation_waivers: tuple[MutationWaiverPolicy, ...] = ()

    def classify_change(self, change: Change) -> Change:
        from conductor._native import candidate_policy_classify_native

        result = json.loads(
            candidate_policy_classify_native(
                json.dumps(
                    {
                        "path": change.path,
                        "old_path": change.old_path,
                        "new_mode": change.new_mode,
                        "old_mode": change.old_mode,
                    }
                ),
                json.dumps(
                    {
                        "class_globs": self.class_globs,
                        "generated_globs": self.generated_globs,
                        "high_risk_globs": self.high_risk_globs,
                    }
                ),
            )
        )
        return Change(
            status=change.status,
            path=change.path,
            old_path=change.old_path,
            old_mode=change.old_mode,
            new_mode=change.new_mode,
            old_oid=change.old_oid,
            new_oid=change.new_oid,
            classes=tuple(result["classes"]),
            risk=result["risk"],
        )

    def active_checks(self, profile: str) -> tuple[CheckPolicy, ...]:
        return tuple(check for check in self.checks if profile in check.profiles)


def _native_json(raw: Any) -> str:
    def convert(value: Any) -> str:
        if isinstance(value, datetime):
            return value.date().isoformat()
        if isinstance(value, date):
            return value.isoformat()
        if isinstance(value, Path):
            return str(value)
        raise TypeError(f"unsupported policy value: {type(value).__name__}")

    return json.dumps(raw, default=convert)


def _date(raw: str | None) -> date | None:
    return None if raw is None else date.fromisoformat(raw)


def _check(raw: dict[str, Any]) -> CheckPolicy:
    return CheckPolicy(
        **{
            **raw,
            "severity": Severity(raw["severity"]),
            **{
                key: tuple(raw[key])
                for key in (
                    "profiles",
                    "classes",
                    "exclude_classes",
                    "command",
                    "version_command",
                )
            },
        }
    )


def _exception(raw: dict[str, Any]) -> ExceptionPolicy:
    return ExceptionPolicy(**{**raw, "expires": date.fromisoformat(raw["expires"])})


def _waiver(raw: dict[str, Any]) -> MutationWaiverPolicy:
    return MutationWaiverPolicy(
        **{
            **raw,
            "expires": date.fromisoformat(raw["expires"]),
            "sources": tuple(WaiverSourceBinding(**entry) for entry in raw["sources"]),
        }
    )


def _value_waiver(raw: dict[str, Any]) -> ValueWaiverPolicy:
    return ValueWaiverPolicy(
        **{
            **raw,
            "nodeids": tuple(raw["nodeids"]),
            "approved_on": date.fromisoformat(raw["approved_on"]),
            "expires": _date(raw["expires"]),
        }
    )


def _parse_value_waivers(raw: Any) -> tuple[ValueWaiverPolicy, ...]:
    from conductor._native import candidate_value_waivers_parse_native

    try:
        rows = json.loads(candidate_value_waivers_parse_native(_native_json(raw)))
    except ValueError as exc:
        raise PolicyError(str(exc)) from exc
    return tuple(_value_waiver(row) for row in rows)


def _fragment(operation: str, raw: Any) -> Any:
    from conductor._native import candidate_policy_fragment_native

    try:
        return json.loads(
            candidate_policy_fragment_native(
                operation, _native_json(raw), datetime.now(UTC).date().isoformat()
            )
        )
    except ValueError as exc:
        raise PolicyError(str(exc)) from exc


def _attribution_value(value: Any, *, field: str) -> str:
    return _fragment("attribution", {"value": value, "field": field})


def _string_tuple(
    value: Any, *, field: str, allow_empty: bool = True
) -> tuple[str, ...]:
    return tuple(
        _fragment(
            "strings", {"value": value, "field": field, "allow_empty": allow_empty}
        )
    )


def _positive_int(value: Any, *, field: str, maximum: int | None = None) -> int:
    return _fragment("positive", {"value": value, "field": field, "maximum": maximum})


def _non_negative_int(value: Any, *, field: str, maximum: int | None = None) -> int:
    return _fragment(
        "nonnegative", {"value": value, "field": field, "maximum": maximum}
    )


def _float_percent(value: Any, *, field: str) -> float:
    return _fragment("percent", {"value": value, "field": field})


def _bool_value(value: Any, *, field: str) -> bool:
    return _fragment("boolean", {"value": value, "field": field})


def _date_value(value: Any, *, field: str) -> date:
    return date.fromisoformat(_fragment("date", {"value": value, "field": field}))


def _parse_check(check_id: str, raw: Any) -> CheckPolicy:
    return _check(_fragment("check", {"id": check_id, "value": raw}))


def _validate_exception_path(path: str) -> None:
    _fragment("exception_path", path)


def _parse_exception(raw: Any) -> ExceptionPolicy:
    return _exception(_fragment("exception", raw))


def _parse_baselines(raw: Any) -> tuple[BaselinePolicy, ...]:
    return tuple(
        BaselinePolicy(
            **{
                **row,
                "classes": tuple(row["classes"]),
                "required_profiles": tuple(row["required_profiles"]),
            }
        )
        for row in _fragment("baselines", raw)
    )


def _intrinsic_classes(
    change: Change,
    *,
    path_override: str | None = None,
    mode_override: str | None = None,
) -> set[str]:
    from conductor._native import candidate_policy_classify_native

    result = json.loads(
        candidate_policy_classify_native(
            json.dumps(
                {
                    "path": path_override or change.path,
                    "new_mode": mode_override or change.new_mode,
                }
            ),
            '{"class_globs":{},"generated_globs":[],"high_risk_globs":[]}',
        )
    )
    return set(result["classes"])


def _validate_policy(policy: Policy) -> None:
    _fragment("validate", asdict(policy))


def load_policy(path: Path) -> Policy:
    from conductor._native import candidate_policy_parse_native

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
    try:
        parsed = json.loads(
            candidate_policy_parse_native(
                _native_json(raw), datetime.now(UTC).date().isoformat()
            )
        )
    except ValueError as exc:
        raise PolicyError(str(exc)) from exc
    return Policy(
        path=path,
        digest=sha256_bytes(raw_bytes),
        schema_version=parsed["schema_version"],
        block_at=Severity(parsed["block_at"]),
        max_workers=parsed["max_workers"],
        cache_ttl_days=parsed["cache_ttl_days"],
        claim_max_age_hours=parsed["claim_max_age_hours"],
        max_file_bytes=parsed["max_file_bytes"],
        max_binary_bytes=parsed["max_binary_bytes"],
        coverage_threshold=parsed["coverage_threshold"],
        high_risk_coverage_threshold=parsed["high_risk_coverage_threshold"],
        baseline_expires=date.fromisoformat(parsed["baseline_expires"]),
        class_globs={
            name: tuple(globs) for name, globs in parsed["class_globs"].items()
        },
        high_risk_globs=tuple(parsed["high_risk_globs"]),
        protected_delete_globs=tuple(parsed["protected_delete_globs"]),
        hot_path_globs=tuple(parsed["hot_path_globs"]),
        generated_globs=tuple(parsed["generated_globs"]),
        checks=tuple(_check(row) for row in parsed["checks"]),
        baselines=tuple(
            BaselinePolicy(
                **{
                    **row,
                    "classes": tuple(row["classes"]),
                    "required_profiles": tuple(row["required_profiles"]),
                }
            )
            for row in parsed["baselines"]
        ),
        exceptions=tuple(_exception(row) for row in parsed["exceptions"]),
        mutation_waivers=tuple(_waiver(row) for row in parsed["mutation_waivers"]),
        value_waivers=tuple(_value_waiver(row) for row in parsed["value_waivers"]),
        tools=tuple(
            ToolPolicy(
                **{
                    **row,
                    "version_command": tuple(row["version_command"]),
                    "required_profiles": tuple(row["required_profiles"]),
                }
            )
            for row in parsed["tools"]
        ),
        expired_exceptions=tuple(
            _exception(row) for row in parsed["expired_exceptions"]
        ),
        expired_mutation_waivers=tuple(
            _waiver(row) for row in parsed["expired_mutation_waivers"]
        ),
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


def expired_entries(policy: Policy) -> dict[str, list[dict[str, str]]]:
    """Expired exceptions and mutation waivers, dropped at load and named in the receipt."""

    def row(
        entry_id: str, entry: ExceptionPolicy | MutationWaiverPolicy
    ) -> dict[str, str]:
        return {
            "id": entry_id,
            "owner": entry.owner,
            "path": entry.path,
            "expires": entry.expires.isoformat(),
        }

    return {
        "expired_exceptions": [
            row(e.exception_id, e) for e in policy.expired_exceptions
        ],
        "expired_mutation_waivers": [
            row(w.waiver_id, w) for w in policy.expired_mutation_waivers
        ],
    }


def unmatched_exceptions(
    policy: Policy,
    examined: Mapping[str, Collection[str]],
    findings: Sequence[Finding],
) -> tuple[dict[str, str], ...]:
    """Exceptions whose own check read their file and found nothing to excuse.

    An exception naming a path no check examined is not dead -- this candidate
    simply did not touch it, and saying so on every run would bury the signal.
    One whose check *did* read the file and produced no finding it covers is
    stale: the code was fixed, or a fingerprint drifted, and what is left reads
    as live governance while excusing nothing. `mut-testing-oversized-func`
    outlived its finding by weeks that way.

    Reported, never blocking. A stale exemption is somebody's debt to remove,
    not a reason to fail the candidate that happened to touch the file.
    """
    matched = {finding.exception_id for finding in findings}
    stale: list[dict[str, str]] = []
    for exception in policy.exceptions:
        if exception.exception_id in matched:
            continue
        seen = examined.get(exception.check_id, ())
        if not any(fnmatch.fnmatchcase(path, exception.path) for path in seen):
            continue
        stale.append(
            {
                "id": exception.exception_id,
                "check": exception.check_id,
                "rule": exception.rule_id or "",
                "path": exception.path,
                "owner": exception.owner,
                "expires": exception.expires.isoformat(),
            }
        )
    return tuple(stale)


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
