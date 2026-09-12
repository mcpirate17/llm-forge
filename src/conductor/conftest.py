"""Shared fixtures for conductor tests that exercise agent hook wiring.

The runtime matrix and the local-AI policy tests verify the shell hooks every
agent launcher (Codex, Claude, Qwen, Grok) routes through the shared guard.
Those hook suites are per-host configuration, absent from a clean checkout, so
the tests build an equivalent suite under ``tmp_path`` and point the code at it
through its ``root`` parameters instead of reading the host's files.
"""

from __future__ import annotations

import importlib
import json
import os
import shlex
import shutil
import subprocess
import sys
import textwrap
from pathlib import Path
from typing import Callable

import pytest

from conductor import active_state
from conductor._project_hooks import register_test_path_guard

REPO_ROOT = Path(__file__).resolve().parents[1]


def pytest_configure(config: pytest.Config) -> None:
    """Register the host's path guard (deliverable 5): fail a test that reads
    outside the exported tree, skip one that reads a declared-absent artifact.
    See conductor/_project_hooks.py."""
    register_test_path_guard(config)


# (agent, read tool, shell tool, extra exports in the pre-edit hook)
_AGENTS: tuple[tuple[str, str, str, str], ...] = (
    ("codex", "Read", "Bash", ""),
    ("claude", "Read", "Bash", ""),
    ("qwen", "read_file", "run_shell_command", "export LOCAL_AI_RUNTIME=1\n"),
    ("grok", "read_file", "run_shell_command", ""),
)
_CONFIG_PATHS = {
    "codex": Path(".codex/hooks.json"),
    "claude": Path(".claude/settings.json"),
    "qwen": Path(".qwen/settings.json"),
    "grok": Path(".grok/hooks/workspace.json"),
}
_PRE_EDIT_HOOK = """#!/usr/bin/env bash
# PreToolUse: route every read and edit through the shared current-work guard.
set -euo pipefail
{exports}export PYTHONPATH={repo}${{PYTHONPATH:+:$PYTHONPATH}}
exec {python} -m conductor.current_work_guard
"""
_PRE_BASH_HOOK = """#!/usr/bin/env bash
# PreToolUse/Bash: deny history-destroying git commands.
set -euo pipefail
payload=$(cat)
case "$payload" in
  *'git reset --hard'*) echo 'BLOCKED: git reset --hard destroys uncommitted work' ;;
esac
"""
_POST_HOOK = """#!/usr/bin/env bash
# PostToolUse: bounded no-op.
set -euo pipefail
cat >/dev/null
echo '{"hookSpecificOutput":{"hookEventName":"PostToolUse"}}'
"""
_OBSIDIAN_SYNC = """#!{python}
# PostToolUse no-op standing in for the vault mirror.
import sys

sys.stdin.read()
print('{{"hookSpecificOutput":{{"hookEventName":"PostToolUse"}}}}')
"""


def _write_program(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")
    path.chmod(0o755)


def _hook_config(
    root: Path, agent: str, read_tool: str, shell_tool: str
) -> dict[str, object]:
    hooks = root / f".{agent}" / "hooks"
    gate = f"GOVERNANCE_OWNER={agent} {root / '.agent_hooks' / 'crg_gate.py'} verify"
    return {
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": read_tool,
                    "hooks": [
                        {"type": "command", "command": str(hooks / "pre-edit.sh")}
                    ],
                },
                {
                    "matcher": shell_tool,
                    "hooks": [{"type": "command", "command": gate}],
                },
            ]
        }
    }


@pytest.fixture
def probe_workspace(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> Callable[[str, str], Path]:
    """Build a throwaway module and a driver test where the probe can reach them.

    Both probe suites need the same six steps and differ only in the source they
    put under test, so the steps were copied between them -- and the reasoning
    behind them was copied too. `pytest.ini` is written because without an inifile
    the nested run walks up to `/` looking for one, which the repo path guard
    rejects; the modules are evicted from `sys.modules` because a second workspace
    in the same session otherwise imports the first one's `fixture_mod`.
    """

    def build(module_src: str, tests_src: str) -> Path:
        (tmp_path / "pytest.ini").write_text("[pytest]\n")
        (tmp_path / "fixture_mod.py").write_text(textwrap.dedent(module_src))
        (tmp_path / "test_fixture_mod.py").write_text(textwrap.dedent(tests_src))
        monkeypatch.chdir(tmp_path)
        monkeypatch.syspath_prepend(str(tmp_path))
        importlib.invalidate_caches()
        for name in ("fixture_mod", "test_fixture_mod"):
            sys.modules.pop(name, None)
        return tmp_path

    return build


@pytest.fixture
def hook_repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A git repository carrying the four agent hook suites the matrix checks."""
    root = tmp_path / "repo"
    root.mkdir()
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    python = shlex.quote(sys.executable)
    for agent, read_tool, shell_tool, exports in _AGENTS:
        hooks = root / f".{agent}" / "hooks"
        hooks.mkdir(parents=True)
        _write_program(
            hooks / "pre-edit.sh",
            _PRE_EDIT_HOOK.format(
                exports=exports, repo=shlex.quote(str(REPO_ROOT)), python=python
            ),
        )
        (root / _CONFIG_PATHS[agent]).write_text(
            json.dumps(_hook_config(root, agent, read_tool, shell_tool), indent=2)
            + "\n",
            encoding="utf-8",
        )
    for agent in ("codex", "claude"):
        hooks = root / f".{agent}" / "hooks"
        _write_program(hooks / "pre-bash.sh", _PRE_BASH_HOOK)
        _write_program(hooks / "post-edit.sh", _POST_HOOK)
        _write_program(
            hooks / "obsidian_sync.py", _OBSIDIAN_SYNC.format(python=sys.executable)
        )
    _write_program(root / ".claude" / "hooks" / "post-bash-graph.sh", _POST_HOOK)
    (root / ".agent_hooks").mkdir()
    # The launcher is a host-checkout artifact, not something the package ships:
    # `conductor init` writes the dispatch launcher, never this one, so a standalone
    # install has none to copy and copying one coupled these tests to the host repo.
    # Synthesize it instead -- its entire contract is to name the checkout it serves
    # and exec the packaged body below under the same interpreter, which is what the
    # tests here actually exercise.
    _write_program(
        root / ".agent_hooks" / "crg_gate.py",
        "#!/usr/bin/env python3\n"
        "import os\n"
        "import sys\n"
        "from pathlib import Path\n"
        "\n"
        "PROJECT_DIR = Path(__file__).resolve().parents[1]\n"
        'os.environ["PROJECT_DIR"] = str(PROJECT_DIR)\n'
        'BODY = PROJECT_DIR / "tooling/hooks/agent/crg_gate.py"\n'
        "os.execv(sys.executable, [sys.executable, str(BODY), *sys.argv[1:]])\n",
    )
    # The launcher above execs the tooling body it finds under its own checkout.
    gate_body = root / "tooling" / "hooks" / "agent" / "crg_gate.py"
    gate_body.parent.mkdir(parents=True)
    shutil.copy(REPO_ROOT / "tooling" / "hooks" / "agent" / "crg_gate.py", gate_body)
    monkeypatch.setenv(
        "GROK_INSPECT_COMMAND",
        f"{python} -m conductor.grok_inspect_stub {shlex.quote(str(root))}",
    )
    monkeypatch.setattr(active_state, "ROOT", root)
    active_state.save_active_state(root / "conductor" / "active_state.json")
    return root


# Tests whose subject is something only a host project has. `_project_hooks.py`
# documents CONDUCTOR_PROJECT_TEST_PLUGIN="" as the standalone case -- conductor
# installed with no host project behind it -- and there these have nothing to
# assert against. Keeping the inventory here rather than as decorators in the test
# files has two payoffs: the whole host-coupling surface of this suite is one list
# you can read and shrink, and each test's own body stays the statement of what it
# proves, unqualified by where it happens to be running.
#
# This list is debt, not architecture. Every entry is a place conductor reaches
# outside its own boundary; the goal is for it to reach zero.
HOST_PROJECT_TESTS: dict[str, str] = {
    "test_candidate_review_cli_policy.py::test_latency_benchmark_uses_isolated_real_git_candidates": (
        "the benchmark's subject is the host governance surface -- GOVERNANCE_PATHS "
        "names .github/CODEOWNERS, AGENTS.md, the Makefile and research/notes"
    ),
    "test_repo_index.py::test_the_index_resolves_every_import_the_ast_matcher_did": (
        "the thresholds describe the host tree's scale; a standalone install has no "
        "host tree to find and the guard has nothing to say"
    ),
    "test_repo_index.py::test_the_index_is_not_degenerate": (
        "same host-tree scale thresholds as the test above"
    ),
}


def pytest_collection_modifyitems(
    config: pytest.Config, items: list[pytest.Item]
) -> None:
    """Skip the host-coupled tests when conductor is installed without a project."""

    if os.environ.get("CONDUCTOR_PROJECT_TEST_PLUGIN") != "":
        return
    for item in items:
        key = f"{Path(str(item.fspath)).name}::{item.originalname or item.name}"
        reason = HOST_PROJECT_TESTS.get(key)
        if reason is not None:
            item.add_marker(pytest.mark.skip(reason=f"no host project: {reason}"))
