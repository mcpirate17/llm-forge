"""`python -m conductor.cost_ledger`: the ledger CLI's Python glue (step 6).

Every computation lives in the native binary (`forge ledger ...`,
`native/forge/src/ledger/`): this module locates that binary exactly the way
`conductor.cost_budget_audit` does (`resolve_forge_binary`), forwards
arguments verbatim, and propagates the child's exit code -- it never
re-implements a computation. Two things are glue rather than forwarding:

* **`rollup` with no paths** fills the defaults `make ledger-rollup` wants:
  this project's own harness transcript directory rolled up into the ledger
  root, joined to this repository's landed commits (`--repo`/`--project`).
  Explicit paths still forward verbatim, untouched.
* **`report`** renders `forge ledger audit`'s JSON verdict as the human table
  `make ledger-report` prints (one line per metric with its status), reusing
  `cost_budget_audit`'s Pydantic models rather than re-parsing the shape.

The small config this shim owns (ledger root, repo path, project, transcript
directory) is Pydantic v2 and resolved as CLI flag > environment variable >
default, mirroring the env precedence `forge ledger` itself applies.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

from pydantic import BaseModel, ConfigDict

from conductor.cost_budget_audit import (
    DEFAULT_WINDOW_DAYS,
    OK_STATUSES,
    AuditResult,
)
from conductor import cost_budget_audit
from conductor.project_init import resolve_forge_binary
from conductor.project_paths import host_root

PROJECTS_DIR = Path.home() / ".claude" / "projects"
FORWARDING_SUBCOMMANDS = ("read", "rollup", "landed", "audit")


class CostLedgerError(RuntimeError):
    """The shim could not run at all (no forge binary, no transcripts dir)."""


class LedgerConfig(BaseModel):
    """Everything the shim's default rollup needs, nothing else.

    `ledger_root` is `None` when neither a flag nor `LEDGER_ROOT` named one:
    the forge subcommand then applies its own built-in default (the same
    resolution `forge ledger rollup` documents for `--out`), so this module
    never restates that path and the boundary checker stays quiet.
    """

    model_config = ConfigDict(extra="forbid")

    ledger_root: Path | None
    repo_path: Path
    project: str
    transcripts_dir: Path
    baseline: Path


def munged_project_name(repo_path: Path) -> str:
    """The harness's directory name for `repo_path`'s transcript project.

    Claude Code encodes a working-directory path as a single directory name
    under `~/.claude/projects/` by replacing every ``/`` with ``-``
    (``/w/proj`` -> ``-w-proj``) -- the same name `forge ledger rollup`'s
    `project_of()` derives as each transcript file's parent directory.
    """

    return str(repo_path.resolve()).replace("/", "-")


def transcripts_dir_for(repo_path: Path) -> Path:
    return PROJECTS_DIR / munged_project_name(repo_path)


def resolve_config(
    *,
    repo_root: Path | None = None,
    ledger_root: Path | None = None,
    transcripts_dir: Path | None = None,
) -> LedgerConfig:
    """CLI flag > environment variable > default, per field.

    `LEDGER_ROOT` is the same variable `forge ledger rollup` and the
    SessionEnd handler read, so one export redirects every writer at once;
    `LEDGER_TRANSCRIPTS` exists for a checkout whose sessions were logged
    under a different path than the repo (a monorepo split, say).
    """

    repo = (repo_root or host_root()).resolve()
    root = ledger_root or os.environ.get("LEDGER_ROOT") or None
    transcripts = (
        transcripts_dir
        or os.environ.get("LEDGER_TRANSCRIPTS")
        or transcripts_dir_for(repo)
    )
    return LedgerConfig(
        ledger_root=Path(root) if root else None,
        repo_path=repo,
        project=Path(transcripts).name,
        transcripts_dir=Path(transcripts),
        baseline=cost_budget_audit.default_baseline_path(repo),
    )


def default_rollup_args(config: LedgerConfig) -> list[str]:
    """The argv `rollup` forwards when the caller named no paths.

    `--project` is the transcript directory's own name so the agent-rollup
    join only ever meets sessions this directory actually holds; `--out` is
    omitted when no ledger root was named, leaving `forge ledger rollup` to
    its own documented default.
    """

    args = [str(config.transcripts_dir)]
    if config.ledger_root is not None:
        args += ["--out", str(config.ledger_root)]
    return args + [
        "--repo",
        str(config.repo_path),
        "--project",
        config.project,
    ]


def run_report(
    *, forge_binary: Path, config: LedgerConfig, window_days: int
) -> int:
    """`make ledger-report`: the audit verdict as a human table.

    Exits with the same rule `conductor.cost_budget_audit.main` applies, so
    `make ledger-report` and `make cost-budget-audit` can never disagree
    about whether the same window passes.
    """

    command = [str(forge_binary), "ledger", "audit", "--baseline", str(config.baseline)]
    if config.ledger_root is not None:
        command += ["--ledger-root", str(config.ledger_root)]
    completed = subprocess.run(
        command + ["--window-days", str(window_days)],
        capture_output=True,
        text=True,
        check=False,
    )
    sys.stderr.write(completed.stderr)
    if completed.returncode == 3:
        return 3
    try:
        result = AuditResult.model_validate(json.loads(completed.stdout))
    except (json.JSONDecodeError, ValueError) as exc:
        raise CostLedgerError(
            f"forge ledger audit produced no JSON (exit {completed.returncode}): {exc}"
        ) from exc

    window = result.window
    print(
        f"cost-budget report  window {window.from_}..{window.to}"
        f" ({window.days} day(s))  overall {result.status}"
    )
    for name, metric in sorted(result.metrics.items()):
        value = "-" if metric.value is None else _format_number(metric.value)
        n = f"  n={metric.n}" if metric.n else ""
        if metric.baseline is None:
            detail = "no baseline"
        else:
            delta = (
                "n/a"
                if metric.delta_pct is None
                else f"{metric.delta_pct:+.1f}%"
            )
            detail = f"baseline {_format_number(metric.baseline)}, {delta}"
        print(f"{name:<26} {metric.status:<12} {value}  ({detail}{n})")
    return 0 if result.status in OK_STATUSES else 1


def _format_number(value: float) -> str:
    return f"{value:,.1f}"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=None,
        help="repository for --repo/--project defaults (default: this checkout)",
    )
    parser.add_argument(
        "--ledger-root",
        type=Path,
        default=None,
        help="ledger root (flag > $LEDGER_ROOT > the forge binary's own default)",
    )
    parser.add_argument(
        "--transcripts",
        type=Path,
        default=None,
        help="transcript directory to roll up by default"
        " (flag > $LEDGER_TRANSCRIPTS > the repo's own harness directory)",
    )
    parser.add_argument(
        "--window-days",
        type=int,
        default=DEFAULT_WINDOW_DAYS,
        help="report window (default: %(default)s)",
    )
    parser.add_argument(
        "subcommand",
        choices=[*FORWARDING_SUBCOMMANDS, "record", "report"],
        help="read/rollup/landed/audit forward to `forge ledger <sub>`;"
        " record is `audit --record`;"
        " report renders the audit verdict as a table",
    )
    parser.add_argument(
        "args",
        nargs=argparse.REMAINDER,
        help="arguments forwarded verbatim to the forge subcommand",
    )
    parsed = parser.parse_args(argv)
    forwarded: list[str] = list(parsed.args)
    if forwarded and forwarded[0] == "--":
        forwarded = forwarded[1:]

    repo_root = parsed.repo_root or host_root()
    forge_binary = resolve_forge_binary(repo_root)
    if forge_binary is None:
        print(
            "cost_ledger: no forge binary found (.tools/bin/forge or PATH)",
            file=sys.stderr,
        )
        return 2

    if parsed.subcommand == "report":
        config = resolve_config(
            repo_root=repo_root,
            ledger_root=parsed.ledger_root,
            transcripts_dir=parsed.transcripts,
        )
        try:
            return run_report(
                forge_binary=forge_binary,
                config=config,
                window_days=parsed.window_days,
            )
        except CostLedgerError as exc:
            print(f"cost_ledger: {exc}", file=sys.stderr)
            return 2

    command: list[str] = [str(forge_binary), "ledger"]
    if parsed.subcommand == "record":
        command += ["audit", "--record"]
    else:
        command.append(parsed.subcommand)
    if parsed.subcommand == "rollup" and not _has_positional_path(forwarded):
        config = resolve_config(
            repo_root=repo_root,
            ledger_root=parsed.ledger_root,
            transcripts_dir=parsed.transcripts,
        )
        if not config.transcripts_dir.is_dir():
            print(
                f"cost_ledger: no transcript directory at"
                f" {config.transcripts_dir} -- pass paths explicitly or set"
                f" LEDGER_TRANSCRIPTS",
                file=sys.stderr,
            )
            return 2
        forwarded = default_rollup_args(config) + forwarded
    command += forwarded
    completed = subprocess.run(command, check=False)
    return completed.returncode


ROLLUP_VALUE_FLAGS = frozenset(
    {"--out", "--repo", "--project", "--cap", "--since", "--last", "--branch"}
)


def _has_positional_path(forwarded: list[str]) -> bool:
    """Whether the caller named any path for `rollup` (then we touch nothing).

    Only a positional counts: a value-taking flag swallows the token after it
    (`--out DIR <path>`), a lone `--` hands everything after it to forge as
    paths by construction, and every other flag (`--dry-run`) stands alone.
    """

    tokens = list(forwarded)
    while tokens:
        token = tokens.pop(0)
        if token == "--":
            return bool(tokens)
        if token.startswith("--") and token.split("=", 1)[0] in ROLLUP_VALUE_FLAGS:
            if "=" not in token and tokens:
                tokens.pop(0)
            continue
        if token.startswith("-"):
            continue
        return True
    return False


if __name__ == "__main__":
    raise SystemExit(main())
