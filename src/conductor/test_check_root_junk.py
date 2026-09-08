"""Contract tests for the repo-root junk guard.

No parametrize: mutation campaigns pin bare node ids, and pytest's
``name[params]`` expansion breaks those pins.
"""

from __future__ import annotations

import pytest

from conductor.check_root_junk import is_forbidden, main


def _run(argv, monkeypatch):
    monkeypatch.setattr("sys.argv", ["guard", *argv])
    return main()


def test_rejects_each_forbidden_glob_at_the_root():
    names = ("BIG_PLAN.md", "HANDOFF.md", "run.log", "metrics.jsonl", "unused.py")
    assert [name for name in names if is_forbidden(name)] == list(names)


def test_ignores_the_same_names_below_the_root():
    assert not is_forbidden("tasks/BIG_PLAN.md")
    assert not is_forbidden("research/reports/run.log")
    assert not is_forbidden("a/b/metrics.jsonl")


def test_allows_the_blessed_do_not_delete_files():
    assert not is_forbidden("COMMANDS_DO_NOT_DELETE.txt")
    assert not is_forbidden("MY_PLAN_DO_NOT_DELETE.md")


def test_accepts_ordinary_root_config():
    assert not is_forbidden("pyproject.toml")
    assert not is_forbidden("CLAUDE.md")
    assert not is_forbidden("Makefile")


def test_matching_is_case_sensitive():
    assert is_forbidden("MY_PLAN.md")
    assert not is_forbidden("my_plan.md")


def test_main_exits_zero_when_nothing_is_forbidden(monkeypatch):
    assert _run(["pyproject.toml", "CLAUDE.md"], monkeypatch) == 0
    assert _run([], monkeypatch) == 0


def test_main_exits_one_and_names_only_the_offender(monkeypatch, capsys):
    assert _run(["pyproject.toml", "PLAN.md"], monkeypatch) == 1
    err = capsys.readouterr().err
    assert "PLAN.md" in err
    assert "pyproject.toml" not in err


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__]))
