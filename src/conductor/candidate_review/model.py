"""Typed review model and canonical receipt serialization."""

from __future__ import annotations

import hashlib
import json
import os
from dataclasses import asdict, dataclass, field
from enum import StrEnum
from pathlib import Path
from typing import Any


class Severity(StrEnum):
    CRITICAL = "critical"
    HIGH = "high"
    MEDIUM = "medium"
    LOW = "low"
    INFO = "info"


SEVERITY_RANK = {
    Severity.INFO: 0,
    Severity.LOW: 1,
    Severity.MEDIUM: 2,
    Severity.HIGH: 3,
    Severity.CRITICAL: 4,
}


class CheckStatus(StrEnum):
    PASSED = "passed"
    FAILED = "failed"
    ERROR = "error"
    SKIPPED = "skipped"
    CACHED = "cached"


@dataclass(frozen=True, slots=True)
class TreeEntry:
    path: str
    mode: str
    object_type: str
    oid: str
    size: int | None = None


@dataclass(frozen=True, slots=True)
class Change:
    status: str
    path: str
    old_path: str | None
    old_mode: str
    new_mode: str
    old_oid: str
    new_oid: str
    classes: tuple[str, ...] = ()
    risk: str = "normal"

    @property
    def deleted(self) -> bool:
        return self.new_mode == "000000"


@dataclass(frozen=True, slots=True)
class Candidate:
    kind: str
    tree_oid: str
    base_tree_oid: str
    base_commit_oid: str | None
    commit_oid: str | None
    target_ref: str | None
    changes: tuple[Change, ...]


@dataclass(slots=True)
class Finding:
    check_id: str
    rule_id: str
    severity: Severity
    message: str
    path: str | None = None
    line: int | None = None
    column: int | None = None
    help: str | None = None
    evidence: dict[str, Any] = field(default_factory=dict)
    fingerprint: str = ""
    exception_id: str | None = None

    def finalize(self) -> Finding:
        if not self.fingerprint:
            stable = {
                "check_id": self.check_id,
                "rule_id": self.rule_id,
                "path": self.path,
                "line": self.line,
                "message": self.message,
            }
            self.fingerprint = sha256_json(stable)[:24]
        return self


@dataclass(slots=True)
class CheckResult:
    check_id: str
    status: CheckStatus
    duration_ms: int
    findings: list[Finding] = field(default_factory=list)
    files: list[str] = field(default_factory=list)
    command: list[str] = field(default_factory=list)
    tool_version: str | None = None
    cache_key: str | None = None
    cache_hit: bool = False
    exit_code: int | None = None
    skipped_reason: str | None = None
    stdout_tail: str = ""
    stderr_tail: str = ""
    metrics: dict[str, Any] = field(default_factory=dict)

    def finalize(self) -> CheckResult:
        self.findings = [finding.finalize() for finding in self.findings]
        return self


@dataclass(slots=True)
class ReviewReceipt:
    schema_version: int
    receipt_id: str
    receipt_digest: str
    surface: str
    profile: str
    decision: str
    candidate: dict[str, Any]
    policy: dict[str, Any]
    engine: dict[str, Any]
    graph: dict[str, Any]
    bypass: dict[str, Any]
    timings: dict[str, Any]
    cache: dict[str, Any]
    baselines: list[dict[str, Any]]
    checks: list[dict[str, Any]]
    findings: list[dict[str, Any]]
    binding: str

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def canonical_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_json(value: Any) -> str:
    return sha256_bytes(canonical_json(value).encode("utf-8"))


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def receipt_payload(receipt: ReviewReceipt) -> dict[str, Any]:
    payload = receipt.to_dict()
    payload["receipt_id"] = ""
    payload["receipt_digest"] = ""
    return payload


def seal_receipt(receipt: ReviewReceipt) -> ReviewReceipt:
    digest = sha256_json(receipt_payload(receipt))
    receipt.receipt_digest = digest
    receipt.receipt_id = f"gr-{digest[:24]}"
    return receipt


def write_json_atomic(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp")
    with temporary.open("w", encoding="utf-8") as handle:
        handle.write(json.dumps(payload, indent=2, sort_keys=True) + "\n")
        handle.flush()
        os.fsync(handle.fileno())
    temporary.replace(path)
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)
