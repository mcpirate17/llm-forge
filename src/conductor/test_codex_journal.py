from __future__ import annotations

from conductor import codex_journal


def test_status_lines_passes_pathspecs_and_filters_sensitive_paths(monkeypatch):
    calls: list[list[str]] = []

    def fake_run_git(args: list[str]) -> str:
        calls.append(args)
        return "\n".join(
            [
                " M AGENTS.md",
                " M secret_token.txt",
                " M research/lab_notebook.db",
            ]
        )

    monkeypatch.setattr(codex_journal, "_run_git", fake_run_git)

    assert codex_journal._status_lines(["AGENTS.md"]) == [" M AGENTS.md"]
    assert calls == [["status", "--short", "--", "AGENTS.md"]]


def test_capped_status_lines_reports_omitted_count():
    status = [" M a.py", " M b.py", " M c.py"]

    assert codex_journal._capped_status_lines(status, 2) == [
        " M a.py",
        " M b.py",
        (
            "... 1 more non-protected changes omitted; "
            "use --path or --max-status to include them."
        ),
    ]


def test_build_entry_accepts_path_scope_and_status_cap(monkeypatch):
    def fake_run_git(args: list[str]) -> str:
        if args == ["branch", "--show-current"]:
            return "master"
        if args == ["rev-parse", "--short", "HEAD"]:
            return "abc1234"
        if args == ["status", "--short", "--", "AGENTS.md", "Makefile"]:
            return "\n".join(
                [" M AGENTS.md", " M Makefile", " M conductor/codex_journal.py"]
            )
        raise AssertionError(f"unexpected git args: {args}")

    monkeypatch.setattr(codex_journal, "_run_git", fake_run_git)

    entry = codex_journal.build_entry(
        "Scoped journal",
        ["pytest conductor/tests/test_codex_journal.py -q"],
        paths=["AGENTS.md", "Makefile"],
        max_status=2,
    )

    assert "- Branch: `master`" in entry
    assert "- HEAD: `abc1234`" in entry
    assert "- ` M AGENTS.md`" in entry
    assert "- ` M Makefile`" in entry
    assert "1 more non-protected changes omitted" in entry
    assert "pytest conductor/tests/test_codex_journal.py -q" in entry
