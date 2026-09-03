"""``python -m conductor init <project-dir>``: scaffold the platform into a project.

Writes, idempotently, everything a foreign checkout needs to run the governance and
efficiency tooling: the dispatcher ``hooks`` block in ``.claude/settings.json``, the
``.claude/hooks/dispatch.py`` launcher, the code-review-graph server in ``.mcp.json``,
a minimal ``conductor/candidate_policy.toml``, the ``conductor/preauthorizations.md``
skeleton, the mutation-campaign directories and registry, and a ``.gitignore`` block.

Merging is by key, never by file: a settings or MCP file keeps every key it already
has, an event or server that is already wired identically is left alone, and one that
is wired *differently* is a conflict -- refused with the file and key named unless
``--force``. Project-owned files (the policy, the preauthorization ledger, the campaign
registry) are created once and never rewritten. ``--dry-run`` prints the unified diff
of every write; ``--check`` exits 1 when a run would write anything or when the hook
doctor finds a dead hook. A run that writes ends with the doctor and fails loud when
any declared hook is dead.
"""

from __future__ import annotations

import argparse
import difflib
import json
import os
import subprocess
import sys
from collections.abc import Sequence
from datetime import date, timedelta
from pathlib import Path
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field

from tooling.hooks.dispatch.paths import TOOLING_ROOT
from tooling.hooks.dispatch.registry import LAUNCHER, settings_block

SETTINGS = ".claude/settings.json"
MCP = ".mcp.json"
POLICY = "conductor/candidate_policy.toml"
PREAUTH = "conductor/preauthorizations.md"
REGISTRY = "conductor/mutation_campaigns/registry.json"
GITIGNORE = ".gitignore"
MCP_SERVER = "code-review-graph"
GITKEEPS = (
    "conductor/mutation_campaigns/receipts/.gitkeep",
    "conductor/mutation_campaigns/patches/.gitkeep",
    ".claude/hooks/project/.gitkeep",
)
MARK_BEGIN = "# >>> conductor init >>>"
MARK_END = "# <<< conductor init <<<"
GITIGNORE_LINES = (
    ".code-review-graph/",
    ".claude/*",
    "!.claude/hooks/",
    "!.claude/hooks/*.py",
    "!.claude/hooks/project/",
    ".mcp.json",
    ".agents/",
    ".current_work.md",
    ".current_work.archive/",
    "conductor/active_state.json",
)
DOCTOR_MODULE = "tooling.hooks.dispatch.doctor"
POLICY_WINDOW_DAYS = 90

Status = Literal["create", "update", "unchanged"]


class InitError(RuntimeError):
    """A refusal: a conflicting key, a malformed file, a dead hook."""


class InitConfig(BaseModel):
    model_config = ConfigDict(frozen=True)

    project_dir: Path
    python: Path = Field(default_factory=lambda: Path(sys.executable))
    force: bool = False
    dry_run: bool = False
    check: bool = False


class FileAction(BaseModel):
    model_config = ConfigDict(frozen=True)

    path: str
    status: Status
    before: str | None
    after: str
    executable: bool = False

    def diff(self) -> str:
        return "".join(
            difflib.unified_diff(
                (self.before or "").splitlines(keepends=True),
                self.after.splitlines(keepends=True),
                fromfile=f"a/{self.path}",
                tofile=f"b/{self.path}",
            )
        )


class InitPlan(BaseModel):
    actions: list[FileAction]
    warnings: list[str] = Field(default_factory=list)

    @property
    def changed(self) -> list[FileAction]:
        return [a for a in self.actions if a.status != "unchanged"]


# ── renderers: (existing text or None) -> desired text ─────────────────────


def _load_json(path: str, text: str | None) -> dict[str, Any]:
    if text is None or not text.strip():
        return {}
    try:
        data = json.loads(text)
    except ValueError as exc:
        raise InitError(f"{path} is not JSON: {exc}") from exc
    if not isinstance(data, dict):
        raise InitError(f"{path} must hold a JSON object")
    return data


def _dump_json(data: dict[str, Any]) -> str:
    return json.dumps(data, indent=2) + "\n"


def _merge_keyed(
    path: str,
    existing: dict[str, Any],
    section: str,
    wanted: dict[str, Any],
    force: bool,
) -> dict[str, Any]:
    """Merge ``wanted`` into ``existing[section]`` key by key.

    A key wired identically is left alone; a key wired differently is a conflict
    unless ``force``; every other key of the file and of the section survives.
    """
    merged = dict(existing)
    current = merged.get(section)
    if current is None:
        current = {}
    if not isinstance(current, dict):
        raise InitError(f"{path}: {section!r} must be a JSON object")
    section_out = dict(current)
    for key, value in wanted.items():
        present = section_out.get(key)
        if present is not None and present != value and not force:
            raise InitError(
                f"{path}: {section}.{key} is already wired differently; "
                "re-run with --force to replace it"
            )
        section_out[key] = value
    merged[section] = section_out
    return merged


def render_settings(existing: str | None, force: bool) -> str:
    data = _load_json(SETTINGS, existing)
    wanted = settings_block()["hooks"]
    return _dump_json(_merge_keyed(SETTINGS, data, "hooks", wanted, force))


def mcp_entry(project_dir: Path, python: Path) -> dict[str, Any]:
    return {
        "command": str(python),
        "args": ["-m", "conductor.crg_server", "--repo", str(project_dir)],
        "cwd": str(project_dir),
        "type": "stdio",
        "env": {"CRG_ROLE": "review"},
    }


def render_mcp(existing: str | None, config: InitConfig) -> str:
    data = _load_json(MCP, existing)
    wanted = {MCP_SERVER: mcp_entry(config.project_dir, config.python)}
    return _dump_json(_merge_keyed(MCP, data, "mcpServers", wanted, config.force))


def render_launcher(python: Path) -> str:
    return f'''#!{python}
"""Launcher only -- the body is tooling.hooks.dispatch (single-process hook dispatcher).

Written by ``conductor init``. One interpreter per hook event: resolves the project
from its own location, exports PROJECT_DIR and calls the dispatcher in-process. A
checkout of the tooling itself runs its own ``tooling/hooks/dispatch``; any other
project runs the installed package. Usage from .claude/settings.json:
``$CLAUDE_PROJECT_DIR/{LAUNCHER} <PreToolUse|PostToolUse|SessionStart|SessionEnd>``.
"""

import os
import sys
from pathlib import Path

PROJECT_DIR = Path(__file__).resolve().parents[2]
os.environ["PROJECT_DIR"] = str(PROJECT_DIR)
if (PROJECT_DIR / "tooling" / "hooks" / "dispatch" / "__init__.py").is_file():
    sys.path.insert(0, str(PROJECT_DIR))

from tooling.hooks.dispatch.__main__ import main  # noqa: E402

raise SystemExit(main())
'''


def render_gitignore(existing: str | None) -> str:
    block = "\n".join((MARK_BEGIN, *GITIGNORE_LINES, MARK_END)) + "\n"
    text = existing or ""
    start = text.find(MARK_BEGIN)
    end = text.find(MARK_END)
    if start >= 0 and end >= start:
        tail = text[end + len(MARK_END) :].lstrip("\n")
        return text[:start] + block + tail
    if text and not text.endswith("\n"):
        text += "\n"
    return text + ("\n" if text else "") + block


def render_policy(today: date) -> str:
    expires = (today + timedelta(days=POLICY_WINDOW_DAYS)).isoformat()
    return f"""# Candidate-review policy written by ``conductor init``: the smallest policy the
# gate accepts. Widen classes, risk globs and checks as the project grows.
schema_version = 1
block_at = "high"
max_workers = 4
cache_ttl_days = 14
claim_max_age_hours = 24
max_file_bytes = 1000000
max_binary_bytes = 5000000
coverage_threshold = 40.0
high_risk_coverage_threshold = 65.0
baseline_expires = {expires}
exceptions = []

[classes]
governance = ["conductor/**", ".claude/**", ".agent_hooks/**", ".github/workflows/**"]
workflow = [".github/workflows/**", ".pre-commit-config.yaml"]

[risk]
high = ["conductor/**", ".claude/**", ".github/workflows/**"]

[paths]
protected_deletes = ["conductor/mutation_campaigns/receipts/**"]
hot = ["**/native/**", "**/kernels/**"]
generated = ["**/generated/**", "**/*.generated.*"]

[checks.candidate-integrity]
kind = "builtin"
profiles = ["fast", "full"]
classes = []
always = true
cache = false
run_on_deletions = true
severity = "critical"
timeout_seconds = 30
memory_mb = 512
max_output_chars = 12000

[checks.config-parse]
kind = "builtin"
attribution = "diff"
profiles = ["fast", "full"]
classes = ["config", "notebook", "workflow"]
cache = true
severity = "critical"
timeout_seconds = 60
memory_mb = 1024
max_output_chars = 12000

[checks.python-ast]
kind = "builtin"
attribution = "diff"
profiles = ["fast", "full"]
classes = ["python"]
cache = true
severity = "high"
timeout_seconds = 90
memory_mb = 2048
max_output_chars = 12000
"""


PREAUTH_TEXT = """# Standing preauthorizations

Grants by the project owner only. Agents read this before starting `R2`/`R3` work.
A grant is in force only when **committed**: `git log -p conductor/preauthorizations.md`
is the authorization record, and an uncommitted edit is inert. Expired is absent; there
is no implicit renewal. A grant never covers a tier it does not name.

To grant: append an entry under **Active** and commit it.

```
### <id> — <one line>
granted-by: <owner>
granted:    YYYY-MM-DD
expires:    YYYY-MM-DD          # required; absent or past = no grant
scope:      <lane, paths, or kind of run this covers>
max-tier:   R2 | R3
ceiling:    <machine-hours per run, and in total>
excludes:   <anything explicitly not covered>
```

Move an entry to **Expired** rather than deleting it.

## Active

_None._

## Expired

_None._
"""

REGISTRY_TEXT = _dump_json(
    {
        "schema_version": 1,
        "enforcement": "changed_tests",
        "test_patterns": ["**/test_*.py", "**/*_test.py"],
        "receipt_directories": ["conductor/mutation_campaigns/receipts"],
        "campaigns": [],
        "engine_adapters": {"python": ["reviewed_unified_diff"]},
    }
)


# ── planning and applying ───────────────────────────────────────────────────


def _read(project_dir: Path, rel: str) -> str | None:
    path = project_dir / rel
    return path.read_text(encoding="utf-8") if path.is_file() else None


def _action(
    rel: str, before: str | None, after: str, *, executable: bool = False
) -> FileAction:
    status: Status = (
        "create" if before is None else "unchanged" if before == after else "update"
    )
    return FileAction(
        path=rel, status=status, before=before, after=after, executable=executable
    )


def _create_once(project_dir: Path, rel: str, text: str) -> FileAction:
    before = _read(project_dir, rel)
    return _action(rel, before, text if before is None else before)


def _crg_importable(python: Path) -> bool:
    probe = subprocess.run(
        [str(python), "-c", "import code_review_graph"],
        capture_output=True,
        timeout=60,
        check=False,
    )
    return probe.returncode == 0


def plan(config: InitConfig, *, today: date | None = None) -> InitPlan:
    root = config.project_dir
    actions = [
        _action(
            SETTINGS,
            _read(root, SETTINGS),
            render_settings(_read(root, SETTINGS), config.force),
        ),
        _action(
            LAUNCHER,
            _read(root, LAUNCHER),
            render_launcher(config.python),
            executable=True,
        ),
        _action(MCP, _read(root, MCP), render_mcp(_read(root, MCP), config)),
        _create_once(root, POLICY, render_policy(today or date.today())),
        _create_once(root, PREAUTH, PREAUTH_TEXT),
        _create_once(root, REGISTRY, REGISTRY_TEXT),
        *(_create_once(root, keep, "") for keep in GITKEEPS),
        _action(
            GITIGNORE, _read(root, GITIGNORE), render_gitignore(_read(root, GITIGNORE))
        ),
    ]
    warnings: list[str] = []
    if not _crg_importable(config.python):
        warnings.append(
            f"code_review_graph is not importable from {config.python}; the "
            f"{MCP_SERVER} MCP server in {MCP} will not start until it is installed "
            "(pip install 'conductor-tooling[graph]')"
        )
    return InitPlan(actions=actions, warnings=warnings)


def apply(plan_: InitPlan, project_dir: Path) -> list[str]:
    """Write every changed file; returns the project-relative paths written."""
    written: list[str] = []
    for action in plan_.changed:
        path = project_dir / action.path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(action.after, encoding="utf-8")
        if action.executable:
            path.chmod(path.stat().st_mode | 0o111)
        written.append(action.path)
    return written


def run_doctor(config: InitConfig) -> int:
    """The hook doctor over the scaffolded settings; its exit status, output shown."""
    # The doctor, and the launcher it exercises, must see the tooling this init ran
    # with: the checkout in the monorepo, site-packages in a foreign install.
    env = dict(os.environ)
    env["PYTHONPATH"] = os.pathsep.join(
        [str(TOOLING_ROOT), *filter(None, [env.get("PYTHONPATH")])]
    )
    proc = subprocess.run(
        [
            str(config.python),
            "-m",
            DOCTOR_MODULE,
            "--project-dir",
            str(config.project_dir),
        ],
        capture_output=True,
        text=True,
        cwd=config.project_dir,
        env=env,
        timeout=600,
        check=False,
    )
    sys.stdout.write(proc.stdout)
    sys.stderr.write(proc.stderr)
    return proc.returncode


def _report(plan_: InitPlan, *, diff: bool) -> None:
    for action in plan_.actions:
        print(f"conductor init | {action.status:9s} {action.path}")
        if diff and action.status != "unchanged":
            sys.stdout.write(action.diff())
    for warning in plan_.warnings:
        print(f"conductor init | WARN {warning}", file=sys.stderr)


def run(config: InitConfig) -> int:
    if not (config.project_dir / ".git").exists():
        raise InitError(f"{config.project_dir} is not a git repository root")
    plan_ = plan(config)
    _report(plan_, diff=config.dry_run)
    if config.dry_run:
        return 0
    if config.check:
        drift = plan_.changed
        if drift:
            print(
                f"conductor init | CHECK FAIL {len(drift)} file(s) would change",
                file=sys.stderr,
            )
            return 1
        return run_doctor(config)
    written = apply(plan_, config.project_dir)
    print(f"conductor init | wrote {len(written)} file(s) under {config.project_dir}")
    if run_doctor(config) != 0:
        raise InitError("hook doctor found a dead hook; the scaffold is not usable")
    return 0


def parse_args(argv: Sequence[str] | None) -> InitConfig:
    parser = argparse.ArgumentParser(
        prog="python -m conductor init", description=__doc__
    )
    parser.add_argument("project_dir", type=Path)
    parser.add_argument(
        "--python",
        type=Path,
        default=Path(sys.executable),
        help="interpreter the launcher and MCP server run under (default: this one)",
    )
    parser.add_argument(
        "--force", action="store_true", help="replace conflicting hook and MCP entries"
    )
    parser.add_argument(
        "--dry-run", action="store_true", help="print the diff, write nothing"
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit 1 if a run would write, or a hook is dead",
    )
    args = parser.parse_args(argv)
    if args.dry_run and args.check:
        parser.error("--dry-run and --check are exclusive")
    return InitConfig(
        project_dir=args.project_dir.resolve(),
        # absolute, never resolved: a venv interpreter is a symlink out of the venv
        python=args.python.absolute(),
        force=args.force,
        dry_run=args.dry_run,
        check=args.check,
    )


def main(argv: Sequence[str] | None = None) -> int:
    config = parse_args(argv)
    try:
        return run(config)
    except InitError as exc:
        print(f"conductor init | REFUSED {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
