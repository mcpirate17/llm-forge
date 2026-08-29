"""Shared immutable types for workspace runtime verification."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import StrEnum
from typing import Any


class ReceiptStatus(StrEnum):
    PASS = "PASS"
    NOT_READY = "NOT_READY"
    FAIL_CLOSED = "FAIL-CLOSED"


@dataclass(frozen=True, slots=True)
class CellReceipt:
    cell_id: str
    status: ReceiptStatus
    detail: str
    required: bool = True
    evidence: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True, slots=True)
class LauncherSpec:
    name: str
    argv: tuple[str, ...]
    config_relpath: str
    config_payload: dict[str, Any]
