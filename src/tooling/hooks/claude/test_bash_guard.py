"""Regression tests for .claude/hooks/_bash_guard.py (command-position deny rules).

The guard is loaded relative to this file so a mutation snapshot tests its own copy.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path

_GUARD = Path(__file__).resolve().with_name("_bash_guard.py")
_spec = importlib.util.spec_from_file_location("bash_guard_under_test", _GUARD)
assert _spec and _spec.loader
guard = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(guard)


def _blocked(command: str) -> bool:
    return guard.check(command) is not None


def test_blocks_force_push_but_not_force_with_lease() -> None:
    assert _blocked("git push --force origin master")
    assert _blocked("git push -f origin master")
    assert not _blocked("git push --force-with-lease origin master")
    assert not _blocked("git push origin master")


def test_blocks_reset_hard_but_not_soft() -> None:
    assert _blocked("git reset --hard HEAD~1")
    assert _blocked("cd /tmp && git reset --hard")
    assert not _blocked("git reset --soft HEAD~1")


def test_blocks_git_clean_short_flags_but_not_dry_run() -> None:
    assert _blocked("git clean -fd")
    assert _blocked("git clean -fdx")
    assert _blocked("git clean --force")
    assert not _blocked("git clean --dry-run")


def test_blocks_recursive_rm_of_dangerous_targets_only() -> None:
    assert _blocked("rm -rf /")
    assert _blocked("rm -rf /home/tim/stuff")
    assert _blocked("rm -rf ~/important")
    assert _blocked("rm -r ../sibling")
    assert not _blocked("rm -rf ./build")
    assert not _blocked("rm -rf research/tmp/scratch")
    assert not _blocked("rm -f /tmp/single_file.txt")


def test_blocks_raw_pip_but_not_uv_or_other_modules() -> None:
    assert _blocked("pip install numpy")
    assert _blocked("python -m pip install numpy")
    assert _blocked("python3 -m pip install numpy")
    assert not _blocked("uv pip install numpy")
    assert not _blocked("python3 -m research.tools.rotate_current_work --apply")


def test_recurses_into_shell_runner_payloads() -> None:
    assert _blocked('bash -c "git push --force"')
    assert _blocked("sh -c 'git reset --hard'")
    assert not _blocked('bash -c "ls -la"')


def test_splits_on_shell_operators() -> None:
    assert _blocked("ls; git reset --hard")
    assert _blocked("true && rm -rf /etc")
    assert _blocked("echo a || git push --force origin x")
    assert not _blocked("ls -la && echo done")


def test_quoted_mentions_are_not_commands() -> None:
    # The 2026-08-08 incident: a banned pattern quoted inside a payload.
    assert not _blocked(
        """echo '{"tool_input":{"command":"git push --force origin master"}}' | ./hook.sh"""
    )
    assert not _blocked('echo "git reset --hard is blocked"')
    assert not _blocked('grep -rn "git clean -fd" docs/')


def test_unparseable_command_falls_back_to_conservative_patterns() -> None:
    # An unbalanced quote defeats shlex; the regex fallback must still deny.
    assert _blocked('git push --force origin master "oops')
    assert _blocked('git reset --hard "oops')
    assert not _blocked('echo "hello')
