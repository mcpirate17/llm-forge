"""Tests for conductor.index_notes -- the notes_fts source-selection bug.

`_source_roots()` used to return EITHER the vault trees OR the repo's own
`research/notes` + `tasks`, never both. On a machine where the vault exists
that left the repo's own notes indexed nowhere. These tests pin the fixed
behaviour: the repo's notes and tasks are always indexed, and the vault
trees are indexed *additionally* when present.
"""

from __future__ import annotations

import sqlite3

import pytest

from conductor import index_notes


def _write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


@pytest.fixture
def repo_trees(tmp_path):
    notes_dir = tmp_path / "repo" / "research" / "notes"
    tasks_dir = tmp_path / "repo" / "tasks"
    _write(notes_dir / "alpha.md", "# Alpha Note\n\nbody of alpha note tfwd marker\n")
    _write(tasks_dir / "beta.md", "# Beta Task\n\nbody of beta task\n")
    return notes_dir, tasks_dir


def _patch_sources(monkeypatch, tmp_path, notes_dir, tasks_dir, *, vault_root):
    """Point every module constant `_source_roots()` reads at temp trees."""
    monkeypatch.setattr(index_notes, "VAULT_ROOT", str(vault_root))
    monkeypatch.setattr(
        index_notes,
        "VAULT_SOURCES",
        (
            ("vault_research", str(vault_root / "research")),
            ("vault_dashboards", str(vault_root / "dashboards")),
            ("vault_runbooks", str(vault_root / "runbooks")),
        ),
    )
    monkeypatch.setattr(index_notes, "TASKS_SOURCE", ("tasks", str(tasks_dir)))
    monkeypatch.setattr(index_notes, "_fallback_notes_source", lambda: ("notes", str(notes_dir)))


def test_source_roots_includes_repo_and_vault_when_vault_present(monkeypatch, tmp_path, repo_trees):
    notes_dir, tasks_dir = repo_trees
    vault_root = tmp_path / "vault"
    _write(vault_root / "research" / "gamma.md", "# Gamma\n\nvault-only note\n")
    _patch_sources(monkeypatch, tmp_path, notes_dir, tasks_dir, vault_root=vault_root)

    roots = index_notes._source_roots()
    sources = {name for name, _ in roots}

    assert "notes" in sources
    assert "tasks" in sources
    assert "vault_research" in sources
    assert str(notes_dir) in {r for _, r in roots}


def test_rebuild_indexes_both_repo_notes_and_vault(monkeypatch, tmp_path, repo_trees):
    notes_dir, tasks_dir = repo_trees
    vault_root = tmp_path / "vault"
    _write(vault_root / "research" / "gamma.md", "# Gamma\n\nvault-only note\n")
    _patch_sources(monkeypatch, tmp_path, notes_dir, tasks_dir, vault_root=vault_root)
    monkeypatch.setattr(index_notes, "REPO", str(tmp_path / "repo"))

    conn = sqlite3.connect(":memory:")
    try:
        n_files, _n_tables = index_notes.rebuild(conn)
        rows = conn.execute("SELECT path, source FROM notes_fts ORDER BY path").fetchall()
        sources = {source for _path, source in rows}
        assert n_files == 3
        assert sources == {"notes", "tasks", "vault_research"}
        # The repo note is discoverable by content -- the bug this guards
        # against made it invisible even though it was written to disk.
        hit = conn.execute(
            "SELECT path FROM notes_fts WHERE notes_fts MATCH ?", ('"tfwd"',)
        ).fetchall()
        assert hit and hit[0][0].endswith("alpha.md")
    finally:
        conn.close()


def test_source_roots_repo_only_when_no_vault(monkeypatch, tmp_path, repo_trees):
    notes_dir, tasks_dir = repo_trees
    missing_vault = tmp_path / "no-such-vault"
    _patch_sources(monkeypatch, tmp_path, notes_dir, tasks_dir, vault_root=missing_vault)

    roots = index_notes._source_roots()
    sources = {name for name, _ in roots}
    assert sources == {"notes", "tasks"}
