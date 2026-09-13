"""Tests for the generic-hook / project-extension seam.

Covers the two halves of the convention documented in AGENTS.md:
``_append_context.py`` (the additive merge) and ``session-start.sh`` (the
dispatch). The shell tests build a throwaway repo root in ``tmp_path`` with
stub ``conductor`` modules, so the generic hook is exercised end to end both
with and without ``.claude/hooks/project/`` present.
"""

from __future__ import annotations

import importlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

HOOKS = Path(__file__).resolve().parent
sys.path.insert(0, str(HOOKS))
import _append_context as ac  # noqa: E402
import obsidian_sync as osync  # noqa: E402

SESSION_START = HOOKS / "session-start.sh"
PAYLOAD = {"session_id": "s1", "hook_event_name": "SessionStart", "source": "startup"}

_STUB_PREAMBLE = (
    "import json\n"
    'print(json.dumps({"hookSpecificOutput": {"hookEventName": "SessionStart",'
    ' "additionalContext": "GENERIC"}}))\n'
)
_STUB_PASSTHROUGH = "import sys\nsys.stdout.write(sys.stdin.read())\n"
_STUB_GATE = "#!/usr/bin/env python3\nimport sys\nsys.stdin.read()\n"


def _fake_root(tmp_path: Path) -> Path:
    """A minimal checkout the generic session-start hook can run inside."""
    root = tmp_path / "repo"
    hooks = root / "tooling" / "hooks" / "claude"
    hooks.mkdir(parents=True)
    for name in ("session-start.sh", "_identity.sh", "_append_context.py", "_prune.sh"):
        shutil.copy2(HOOKS / name, hooks / name)
    (hooks / "session-start.sh").chmod(0o755)
    (hooks / "_append_context.py").chmod(0o755)
    agent_hooks = root / "tooling" / "hooks" / "agent"
    agent_hooks.mkdir()
    (agent_hooks / "crg_gate.py").write_text(_STUB_GATE)
    (agent_hooks / "crg_gate.py").chmod(0o755)
    conductor = root / "conductor"
    conductor.mkdir()
    (conductor / "__init__.py").write_text("")
    (conductor / "active_state.py").write_text("")
    (conductor / "session_preamble.py").write_text(_STUB_PREAMBLE)
    (conductor / "context_telemetry.py").write_text(_STUB_PASSTHROUGH)
    return root


def _write_project_hook(root: Path, body: str) -> Path:
    project = root / ".claude" / "hooks" / "project"
    project.mkdir(parents=True, exist_ok=True)
    hook = project / "session-start.sh"
    hook.write_text(body)
    hook.chmod(0o755)
    return hook


def _run_session_start(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(root / "tooling" / "hooks" / "claude" / "session-start.sh")],
        input=json.dumps(PAYLOAD),
        capture_output=True,
        text=True,
        cwd=str(root),
        env={**os.environ, "A2A_AGENT_NAME": "", "PYTHONPATH": ""},
    )


def _context(proc: subprocess.CompletedProcess[str]) -> str:
    return json.loads(proc.stdout)["hookSpecificOutput"]["additionalContext"]


def test_merge_appends_without_replacing_generic_context() -> None:
    payload = {
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": "generic",
        }
    }
    out = ac.merge(payload, "project")
    assert out["hookSpecificOutput"]["additionalContext"] == "generic\n\nproject"
    assert out["hookSpecificOutput"]["hookEventName"] == "SessionStart"
    assert payload["hookSpecificOutput"]["additionalContext"] == "generic"


def test_merge_creates_the_field_and_ignores_empty_or_bad_input() -> None:
    out = ac.merge({"hookSpecificOutput": {"hookEventName": "SessionStart"}}, " p \n")
    assert out["hookSpecificOutput"]["additionalContext"] == "p"
    payload = {"hookSpecificOutput": {"additionalContext": "generic"}}
    assert ac.merge(payload, "   ") is payload
    assert ac.merge("not a dict", "p") == "not a dict"


def test_append_context_cli_passes_through_when_unset(tmp_path: Path) -> None:
    raw = '{"hookSpecificOutput": {"additionalContext": "generic"}}'
    env = {k: v for k, v in os.environ.items() if k != "PROJECT_HOOK_CONTEXT"}
    proc = subprocess.run(
        [sys.executable, str(HOOKS / "_append_context.py")],
        input=raw,
        capture_output=True,
        text=True,
        env=env,
        check=True,
    )
    assert proc.stdout == raw
    proc = subprocess.run(
        [sys.executable, str(HOOKS / "_append_context.py")],
        input="not json",
        capture_output=True,
        text=True,
        env={**env, "PROJECT_HOOK_CONTEXT": "p"},
        check=True,
    )
    assert proc.stdout == "not json"


def test_session_start_runs_generic_only_when_project_is_absent(
    tmp_path: Path,
) -> None:
    root = _fake_root(tmp_path)
    assert not (root / ".claude" / "hooks" / "project").exists()
    proc = _run_session_start(root)
    assert proc.returncode == 0, proc.stderr
    assert _context(proc) == "GENERIC"


def test_session_start_appends_the_project_extension_output(tmp_path: Path) -> None:
    root = _fake_root(tmp_path)
    marker = root / "stdin.json"
    _write_project_hook(
        root,
        "#!/usr/bin/env bash\nset -euo pipefail\n"
        f"cat > {marker}\n"
        'echo "PROJECT-SIDE"\n'
        'echo "root=$REPO_ROOT" >&2\n',
    )
    proc = _run_session_start(root)
    assert proc.returncode == 0, proc.stderr
    assert _context(proc) == "GENERIC\n\nPROJECT-SIDE"
    assert json.loads(marker.read_text()) == PAYLOAD
    assert f"root={root}" in proc.stderr


def test_session_start_survives_a_failing_project_extension(tmp_path: Path) -> None:
    root = _fake_root(tmp_path)
    _write_project_hook(root, "#!/usr/bin/env bash\necho noise\nexit 3\n")
    proc = _run_session_start(root)
    assert proc.returncode == 0, proc.stderr
    assert _context(proc) == "GENERIC"
    assert "project extension failed" in proc.stderr


def test_session_start_ignores_a_non_executable_project_file(tmp_path: Path) -> None:
    root = _fake_root(tmp_path)
    hook = _write_project_hook(root, "#!/usr/bin/env bash\necho PROJECT-SIDE\n")
    hook.chmod(0o644)
    proc = _run_session_start(root)
    assert proc.returncode == 0, proc.stderr
    assert _context(proc) == "GENERIC"


def test_project_extension_prunes_only_what_it_is_given(tmp_path: Path) -> None:
    """_prune.sh is generic: the caller names every directory it touches."""
    stale = tmp_path / "scratch"
    (stale / "old").mkdir(parents=True)
    (stale / "old.txt").write_text("x")
    (stale / "new.txt").write_text("x")
    os.utime(stale / "old", (0, 0))
    os.utime(stale / "old.txt", (0, 0))
    script = tmp_path / "prune.sh"
    script.write_text(
        "#!/usr/bin/env bash\nset -euo pipefail\n"
        f"source {HOOKS / '_prune.sh'}\n"
        f'prune_root_files "{stale}" 7 scratch\n'
        f'prune_stale_subdirs "{stale}" 7 scratch\n'
    )
    proc = subprocess.run(["bash", str(script)], capture_output=True, text=True)
    assert proc.returncode == 0, proc.stderr
    assert not (stale / "old.txt").exists() and not (stale / "old").exists()
    assert (stale / "new.txt").exists()


def test_obsidian_sync_resolves_the_repo_root_from_its_own_location(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.delenv("CLAUDE_PROJECT_DIR", raising=False)
    root = osync.repo_root()
    module = root / "src" / "tooling" / "hooks" / "claude" / "obsidian_sync.py"
    assert module.is_file()
    assert root == HOOKS.parents[3]
    # Installed outside a checkout: git cannot answer, the layout still can.
    stray = tmp_path / "elsewhere" / "tooling" / "hooks" / "claude"
    stray.mkdir(parents=True)
    copy = stray / "obsidian_sync.py"
    shutil.copy2(HOOKS / "obsidian_sync.py", copy)
    proc = subprocess.run(
        [
            sys.executable,
            "-c",
            f"import runpy,sys; sys.argv=['x']; "
            f"m=runpy.run_path({str(copy)!r}); print(m['REPO_ROOT'])",
        ],
        capture_output=True,
        text=True,
        cwd=str(tmp_path),
        env={k: v for k, v in os.environ.items() if k != "CLAUDE_PROJECT_DIR"},
    )
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout.strip() == str(tmp_path / "elsewhere")


def test_obsidian_sync_honors_project_dir_and_notes_env(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    notes = tmp_path / "elsewhere" / "notes"
    notes.mkdir(parents=True)
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(tmp_path))
    monkeypatch.setenv("WORKSPACE_NOTES_DIR", "elsewhere/notes")
    monkeypatch.setenv("OBSIDIAN_VAULT_ROOT", str(tmp_path / "vault"))
    reloaded = importlib.reload(osync)
    try:
        assert reloaded.REPO_ROOT == tmp_path.resolve()
        assert reloaded.NOTES_SOURCE == tmp_path / "elsewhere" / "notes"
        assert reloaded.RESEARCH_VAULT == tmp_path / "vault" / "research"
        assert reloaded.VAULT_ROOT == tmp_path / "vault" / "claude"
        assert reloaded.MEMORY_ROOT.name == "memory"
        slug = str(tmp_path.resolve()).replace("/", "-").replace("_", "-")
        assert reloaded.MEMORY_ROOT.parent.name == slug.replace(".", "-")
        assert "/" not in reloaded.MEMORY_ROOT.parent.name
    finally:
        monkeypatch.undo()
        importlib.reload(osync)


def test_obsidian_sync_defaults_notes_to_research_notes_under_the_root(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(tmp_path))
    monkeypatch.delenv("WORKSPACE_NOTES_DIR", raising=False)
    reloaded = importlib.reload(osync)
    try:
        assert reloaded.NOTES_SOURCE == tmp_path.resolve() / "research" / "notes"
    finally:
        monkeypatch.undo()
        importlib.reload(osync)


def test_obsidian_sync_honors_the_launcher_project_dir(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The exec launcher exports PROJECT_DIR; CLAUDE_PROJECT_DIR still wins."""
    monkeypatch.delenv("CLAUDE_PROJECT_DIR", raising=False)
    monkeypatch.setenv("PROJECT_DIR", str(tmp_path / "launched"))
    assert osync.repo_root() == (tmp_path / "launched").resolve()
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(tmp_path / "claude"))
    assert osync.repo_root() == (tmp_path / "claude").resolve()


def test_obsidian_sync_fails_loud_when_the_notes_dir_is_missing(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(osync, "NOTES_SOURCE", tmp_path / "absent")
    monkeypatch.setattr(sys, "argv", ["obsidian_sync.py", "sync-notes"])
    with pytest.raises(SystemExit) as exc:
        osync.cmd_sync_notes()
    assert exc.value.code == 1
    assert "WORKSPACE_NOTES_DIR" in capsys.readouterr().err
