"""Contract tests for the research/notes/ data-file guard.

No parametrize: mutation campaigns pin bare node ids, and pytest's
``name[params]`` expansion breaks those pins.
"""

from __future__ import annotations

import pytest

from conductor.check_json_in_notes import is_forbidden, main


def _run(argv, monkeypatch):
    # main() resolves the notes root of the workspace it runs in; pin it so the
    # test answers for a monorepo-shaped host regardless of this repo's own
    # `notes_root` configuration.
    monkeypatch.setenv("CONDUCTOR_NOTES_ROOT", "research/notes")
    monkeypatch.setattr("sys.argv", ["guard", *argv])
    return main()


def test_rejects_data_at_the_top_level():
    paths = (
        "research/notes/a.json",
        "research/notes/b.jsonl",
        "research/notes/c.csv",
    )
    assert [path for path in paths if is_forbidden(path)] == list(paths)


def test_keeps_the_knowledge_tree_writable():
    assert not is_forbidden("research/notes/kb_landing_and_gate.md")
    assert not is_forbidden("research/notes/dead_code_audit_2026-08-27.md")


def test_exempts_subdirectories():
    assert not is_forbidden("research/notes/mixer_fingerprint/run.json")
    assert not is_forbidden("research/notes/archive/old.csv")


def test_ignores_data_outside_the_notes_tree():
    assert not is_forbidden("research/data/inputs.json")
    assert not is_forbidden("research/notes_extra/x.json")
    assert not is_forbidden("notes/x.json")


def test_main_exits_zero_when_nothing_is_forbidden(monkeypatch):
    assert _run(["research/data/inputs.json", "pyproject.toml"], monkeypatch) == 0
    assert _run([], monkeypatch) == 0


def test_main_exits_one_and_names_only_the_offender(monkeypatch, capsys):
    argv = ["research/data/inputs.json", "research/notes/bulk.json"]
    assert _run(argv, monkeypatch) == 1
    err = capsys.readouterr().err
    assert "research/notes/bulk.json" in err
    assert "research/data/inputs.json" not in err


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__]))
