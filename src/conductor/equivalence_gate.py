"""The equivalence probe's command line and host configuration.

slop_core does the work: `equivalence_gate` probes each module's units in a child
interpreter against its driver tests, `equivalence_report` renders the result and
`equivalence_backlog` folds results into the ledger. This reads the command line and
the host's ``[tool.forge.equivalence_probe]`` table:

    waivers             JSON ``{"waivers": [{module, rule, qualname}]}``, tracked
    ledger, report      where ``backlog`` keeps the ledger and writes the report
    run_artifacts       where each land's gate run is left for ``backlog`` to fold in
    per_module_timeout  seconds one module's child may run (default 180)
    jobs                children at once (default a quarter of the cores, at most 8)

Relative paths are against the host root, the directory of that pyproject. A host
without the table gets no waivers and no run artifacts, and ``backlog`` must be told
where the ledger is. A host with ``[tool.forge.test_record]`` lines drives each unit
with the recorded tests that ran it, whose recorded seconds may take half a module's
timeout; a unit no such test fits is reported incomplete. Without, with the test files
that import it.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tomllib
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

from conductor._native import (
    equivalence_targets_native,
    slop_core,
    test_record_config_native,
)

_KEYS: dict[str, type | tuple[type, ...]] = {
    "waivers": str,
    "ledger": str,
    "report": str,
    "run_artifacts": str,
    "per_module_timeout": (int, float),
    "jobs": int,
}
_TEST_DIRS = {"test", "tests"}


@dataclass(frozen=True, slots=True)
class Host:
    root: Path
    store: str | None
    table: dict[str, object]

    def path(self, key: str, base: Path | None = None) -> str | None:
        """The table's `key` against `base`, the host root by default, or None."""
        value = self.table.get(key)
        if value is None:
            return None
        return str((base or self.root) / Path(str(value)).expanduser())

    @property
    def timeout(self) -> float:
        return float(self.table.get("per_module_timeout", 180))  # type: ignore[arg-type]

    @property
    def jobs(self) -> int | None:
        return self.table.get("jobs")  # type: ignore[return-value]


def host(start: Path) -> Host:
    """The host `start` is in: its root, its test record store, and its table."""
    config = test_record_config_native(str(start))
    if config is None:
        root, store = Path(start).resolve(), None
    else:
        root, store = Path(config[0]), config[1] if config[2] else None
    pyproject = root / "pyproject.toml"
    table: dict[str, object] = {}
    if pyproject.is_file():
        tool = tomllib.loads(pyproject.read_text()).get("tool", {})
        table = tool.get("forge", {}).get("equivalence_probe", {})
    for key, value in table.items():
        kind = _KEYS.get(key)
        if kind is None or isinstance(value, bool) or not isinstance(value, kind):
            raise ValueError(
                f"{pyproject}: [tool.forge.equivalence_probe] {key} = {value!r} "
                "is not a known key of its type"
            )
    return Host(root, store, table)


def probe(
    found: Host,
    root: Path,
    changes: Sequence[tuple[str, list[int] | None]],
    jobs: int | None = None,
) -> str:
    """The gate's summary JSON for `changes` under `root`, the host root or a copy of it."""
    targets = equivalence_targets_native(
        str(root), found.store, list(changes), found.timeout
    )
    return slop_core().equivalence_gate(
        str(root),
        sys.executable,
        targets,
        found.path("waivers", root),
        found.timeout,
        jobs or found.jobs,
    )


def _git(root: Path, *args: str) -> list[str]:
    out = subprocess.run(
        ["git", *args], cwd=root, capture_output=True, check=True
    ).stdout
    return [name for name in out.decode().split("\0") if name]


def _is_source(path: str) -> bool:
    parts = Path(path).parts
    return (
        path.endswith(".py")
        and not parts[-1].startswith("test_")
        and parts[-1] != "conftest.py"
        and not _TEST_DIRS.intersection(parts[:-1])
    )


def _modules(root: Path, args: argparse.Namespace) -> list[str]:
    if args.module:
        return args.module
    if args.all:
        names = _git(root, "ls-files", "-z", "--", "*.py")
    else:
        names = _git(
            root,
            "diff",
            "--name-only",
            "-z",
            "--diff-filter=d",
            "--merge-base",
            args.base,
        )
    return sorted(
        name for name in names if _is_source(name) and (root / name).is_file()
    )


def _gate(args: argparse.Namespace) -> int:
    found = host(args.root)
    modules = _modules(found.root, args)
    summary = probe(
        found, found.root, [(module, None) for module in modules], args.jobs
    )
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(summary + "\n")
    print(slop_core().equivalence_report(summary), end="")
    return 1 if args.enforce and json.loads(summary)["blocking"] else 0


def _backlog(args: argparse.Namespace) -> int:
    found = host(args.root)
    ledger = args.ledger or found.path("ledger")
    report = args.report or found.path("report")
    if ledger is None or report is None:
        raise SystemExit(
            "backlog: no ledger or report; pass --ledger and --report or set them in "
            "[tool.forge.equivalence_probe]"
        )
    runs = found.path("run_artifacts")
    recorded = sorted(Path(runs).glob("gate-*.json")) if runs else []
    summaries = [str(path) for path in args.summaries] + [str(p) for p in recorded]
    print(slop_core().equivalence_backlog(summaries, ledger, report, args.dry_run))
    if not args.dry_run:
        for path in recorded:
            path.unlink()
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="conductor.equivalence_gate")
    commands = parser.add_subparsers(dest="command", required=True)
    gate = commands.add_parser("gate", help="probe changed, named or all modules")
    gate.add_argument("--root", type=Path, default=Path.cwd())
    gate.add_argument(
        "--base", default="origin/main", help="changed since this revision"
    )
    gate.add_argument("--module", action="append", default=[], help="probe this module")
    gate.add_argument("--all", action="store_true", help="probe every tracked module")
    gate.add_argument("--jobs", type=int, help="children at once")
    gate.add_argument("--json", type=Path, help="write the summary here")
    gate.add_argument(
        "--enforce", action="store_true", help="exit 1 on a blocking finding"
    )
    gate.set_defaults(run=_gate)
    backlog = commands.add_parser(
        "backlog", help="fold summaries, and every recorded gate run, into the ledger"
    )
    backlog.add_argument("summaries", nargs="*", type=Path)
    backlog.add_argument("--root", type=Path, default=Path.cwd())
    backlog.add_argument("--ledger")
    backlog.add_argument("--report")
    backlog.add_argument("--dry-run", action="store_true")
    backlog.set_defaults(run=_backlog)
    args = parser.parse_args(argv)
    return args.run(args)


if __name__ == "__main__":
    raise SystemExit(main())
