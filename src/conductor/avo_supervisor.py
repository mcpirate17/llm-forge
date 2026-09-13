#!/usr/bin/env python3
"""Anti-Stagnation Supervisor for AVO Evolutionary Variation.

Tracks consecutive rejected attempts across variation steps, monitors plateaus,
and issues structured strategy pivots (and optional A2A alerts) to keep the
agent exploration progressing autonomously.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import sys
import tempfile
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Final

from conductor.project_paths import host_root

ROOT: Final[Path] = host_root()
AVO_STATE_PATH: Final[Path] = ROOT / "conductor" / "avo_runtime_state.json"

DEFAULT_STRATEGY_PIVOTS: Final[tuple[str, ...]] = (
    "PIVOT-ARCH: Shift focus from macro architecture to micro-architectural scheduling (instruction overlap, memory fence reduction).",
    "PIVOT-REG: Analyze register pressure and shared memory bank conflicts. Are values spilling to local memory?",
    "PIVOT-BRANCH: Check branch divergence. Can conditional control flow in inner loops be replaced with branchless predicated selects?",
    "PIVOT-ABLATE: Perform isolated single-component ablation before attempting multi-lever modifications.",
    "PIVOT-CONDITIONING: Check gradient norm and condition number of scalar gains across phase boundaries.",
)


@dataclass
class StagnationReport:
    stagnated: bool
    consecutive_rejections: int
    patience: int
    strategy_hint: str | None = None
    alert_payload: dict[str, Any] | None = None

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


class StagnationSupervisor:
    """Monitors variation step outcomes and generates steering directives on stagnation."""

    def __init__(self, patience: int = 4, state_path: Path = AVO_STATE_PATH) -> None:
        self.patience = patience
        self.state_path = state_path

    def load_rejection_count(self) -> int:
        if not self.state_path.exists():
            return 0
        try:
            data = json.loads(self.state_path.read_text(encoding="utf-8"))
            return int(data.get("stagnation_counter", 0))
        except (OSError, json.JSONDecodeError, ValueError):
            return 0

    def save_rejection_count(self, count: int) -> None:
        try:
            data: dict[str, Any] = {}
            if self.state_path.exists():
                loaded = json.loads(self.state_path.read_text(encoding="utf-8"))
                if isinstance(loaded, dict):
                    data = loaded
            data["stagnation_counter"] = count
            data["last_updated"] = dt.datetime.now(dt.timezone.utc).isoformat()
            self.state_path.parent.mkdir(parents=True, exist_ok=True)
            fd, raw_temp = tempfile.mkstemp(
                dir=self.state_path.parent,
                prefix=f".{self.state_path.name}.",
                suffix=".tmp",
            )
            temporary = Path(raw_temp)
            try:
                with os.fdopen(fd, "w", encoding="utf-8") as handle:
                    json.dump(data, handle, indent=2)
                    handle.write("\n")
                    handle.flush()
                    os.fsync(handle.fileno())
                os.replace(temporary, self.state_path)
            finally:
                temporary.unlink(missing_ok=True)
        except (OSError, json.JSONDecodeError, TypeError):
            pass

    def record_step(
        self,
        improved: bool,
        lane: str = "active_lane",
        details: dict[str, Any] | None = None,
    ) -> StagnationReport:
        """Record variation step outcome and return a StagnationReport."""
        if improved:
            self.save_rejection_count(0)
            return StagnationReport(
                stagnated=False,
                consecutive_rejections=0,
                patience=self.patience,
                strategy_hint=None,
            )

        current = self.load_rejection_count() + 1
        self.save_rejection_count(current)

        if current >= self.patience:
            pivot_idx = (current - self.patience) % len(DEFAULT_STRATEGY_PIVOTS)
            hint = DEFAULT_STRATEGY_PIVOTS[pivot_idx]
            alert_payload = {
                "kind": "stagnation-alert",
                "lane": lane,
                "consecutive_rejections": current,
                "patience": self.patience,
                "strategy_hint": hint,
                "details": details or {},
                "timestamp": dt.datetime.now(dt.timezone.utc).isoformat(),
            }
            return StagnationReport(
                stagnated=True,
                consecutive_rejections=current,
                patience=self.patience,
                strategy_hint=hint,
                alert_payload=alert_payload,
            )

        return StagnationReport(
            stagnated=False,
            consecutive_rejections=current,
            patience=self.patience,
            strategy_hint=None,
        )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="AVO Stagnation Supervisor")
    parser.add_argument(
        "--record",
        choices=["improved", "rejected"],
        help="Record a step result (improved or rejected)",
    )
    parser.add_argument(
        "--patience",
        type=int,
        default=4,
        help="Number of consecutive rejections before triggering stagnation alert",
    )
    parser.add_argument(
        "--status",
        action="store_true",
        help="Check current stagnation status",
    )
    args = parser.parse_args(argv)

    supervisor = StagnationSupervisor(patience=args.patience)

    if args.record:
        improved = args.record == "improved"
        report = supervisor.record_step(improved=improved)
        print(json.dumps(report.to_dict(), indent=2))
        return 0

    count = supervisor.load_rejection_count()
    report = StagnationReport(
        stagnated=count >= args.patience,
        consecutive_rejections=count,
        patience=args.patience,
    )
    print(json.dumps(report.to_dict(), indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
