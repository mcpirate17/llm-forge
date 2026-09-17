"""Cross-language parity: Python `conductor.index_notes` vs the Rust
`forge notes` port (`native/forge/src/notes_index.rs`).

Builds one fixture tree (a host repo with `research/notes/` + `tasks/`, and a
fake Obsidian vault with `research/`/`dashboards/`/`runbooks/`), indexes it
with both implementations into separate sqlite databases, and asserts
identical `notes_fts`/`note_tables` content and identical search results.
Any mismatch found here is a Rust bug to fix in `notes_index.rs` -- the
Python module is the reference implementation and is never adjusted to match
Rust.

The Python side runs in-process against a monkeypatched module (its source
roots, vault root and REPO are all module-level constants derived from this
checkout, not CLI flags) rather than shelling out to
`python -m conductor.index_notes`, whose `TASKS_SOURCE`/`VAULT_ROOT` and
resolved notes database all point at *this repo's own* tree -- a subprocess
call would read/write the real database instead of the fixture. The Rust side
does have `--host`/`--vault`/`--db` flags, so it runs as the real
`forge notes` binary via subprocess, which is the actual CI/user path.
"""

from __future__ import annotations

import json
import shutil
import sqlite3
import subprocess
from pathlib import Path

import pytest

from conductor import index_notes

REPO_ROOT = Path(__file__).resolve().parents[2]
FORGE_CRATE = REPO_ROOT / "native" / "forge"
FORGE_BIN_DIR = FORGE_CRATE / "target" / "debug"

TABLE_TITLED = "\n| a | b |\n|---|---|\n| 1 | 2 |\n"
TABLE_UNTITLED = "\n| x | y | z |\n|---|---|---|\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |\n"


def _forge_bin_dir() -> str | None:
    """Build `forge` once and return its target/debug dir, or None if the
    binary is not there afterward (no cargo, or a build failure)."""
    if shutil.which("cargo") is None:
        return None
    subprocess.run(
        ["cargo", "build"],
        cwd=FORGE_CRATE,
        capture_output=True,
        text=True,
        timeout=300,
        check=False,
    )
    return str(FORGE_BIN_DIR) if (FORGE_BIN_DIR / "forge").exists() else None


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def _build_fixture(tmp_path: Path) -> tuple[Path, Path]:
    host = tmp_path / "host"
    vault = tmp_path / "vault"

    for i in range(1, 9):
        _write(
            host / "research" / "notes" / f"note{i}.md",
            f"# Note {i}\n\nbody text {i} apple\n",
        )
    # one note titled by its own first heading, carrying a table
    _write(
        host / "research" / "notes" / "note_titled.md",
        "# Titled Heading\n\nprose about apple banana\n" + TABLE_TITLED,
    )
    # one note with no heading at all (falls back to the filename), also a table
    _write(
        host / "research" / "notes" / "note_untitled.md",
        "prose with no heading, mentions apple\n" + TABLE_UNTITLED,
    )

    _write(host / "tasks" / "task1.md", "# Task One\n\ntask body apple\n")
    _write(host / "tasks" / "task2.md", "# Task Two\n\ntask body banana\n")
    _write(
        host / "tasks" / "audit" / "excluded.md",
        "# Excluded\n\nshould never be indexed apple\n",
    )

    for i in range(1, 5):
        _write(
            vault / "research" / f"vnote{i}.md",
            f"# Vault Note {i}\n\nvault body apple {i}\n",
        )
    _write(
        vault / "dashboards" / "dash1.md", "# Dashboard One\n\ndashboard body apple\n"
    )
    _write(vault / "runbooks" / "runbook1.md", "# Runbook One\n\nrunbook body apple\n")

    return host, vault


def _index_python(
    host: Path, vault: Path, db_path: Path, monkeypatch: pytest.MonkeyPatch
) -> tuple[int, int]:
    db_path.parent.mkdir(parents=True, exist_ok=True)
    db_path.touch()
    monkeypatch.setattr(index_notes, "REPO", str(host))
    monkeypatch.setattr(index_notes, "VAULT_ROOT", str(vault))
    monkeypatch.setattr(index_notes, "TASKS_SOURCE", ("tasks", str(host / "tasks")))
    monkeypatch.setattr(
        index_notes,
        "VAULT_SOURCES",
        (
            ("vault_research", str(vault / "research")),
            ("vault_dashboards", str(vault / "dashboards")),
            ("vault_runbooks", str(vault / "runbooks")),
        ),
    )
    monkeypatch.setattr(
        index_notes,
        "_fallback_notes_source",
        lambda: ("notes", str(host / "research" / "notes")),
    )
    conn = sqlite3.connect(str(db_path))
    try:
        return index_notes.rebuild(conn)
    finally:
        conn.close()


def _index_rust(forge_bin_dir: str, host: Path, vault: Path, db_path: Path) -> None:
    db_path.parent.mkdir(parents=True, exist_ok=True)
    db_path.touch()
    forge = str(Path(forge_bin_dir) / "forge")
    result = subprocess.run(
        [
            forge,
            "notes",
            "index",
            "--host",
            str(host),
            "--vault",
            str(vault),
            "--db",
            str(db_path),
        ],
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert result.returncode == 0, f"forge notes index failed: {result.stderr}"


def _notes_fts_rows(db_path: Path) -> list[tuple]:
    conn = sqlite3.connect(str(db_path))
    try:
        return conn.execute(
            "SELECT path, source, title, body, mtime FROM notes_fts ORDER BY path, source"
        ).fetchall()
    finally:
        conn.close()


def _note_tables_rows(db_path: Path) -> list[tuple]:
    """All content columns of `note_tables`, ordered deterministically.

    `id` (autoincrement) and `ingested_at` (wall-clock rebuild time) are
    excluded: they are bookkeeping, not content, and `ingested_at` differs
    by construction between two separate rebuild runs.
    """
    conn = sqlite3.connect(str(db_path))
    try:
        return conn.execute(
            """SELECT source, path, note, table_idx, section_heading, n_cols,
                      n_rows, headers_json, rows_json
                 FROM note_tables ORDER BY note, table_idx"""
        ).fetchall()
    finally:
        conn.close()


@pytest.fixture()
def forge_bin_dir() -> str:
    bin_dir = _forge_bin_dir()
    if bin_dir is None:
        pytest.skip("forge binary not buildable/available (no cargo, or build failed)")
    return bin_dir


def test_python_and_forge_index_identically(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, forge_bin_dir: str
) -> None:
    host, vault = _build_fixture(tmp_path)

    db_py = tmp_path / "py.db"
    db_rs = tmp_path / "rs.db"

    n_files_py, n_tables_py = _index_python(host, vault, db_py, monkeypatch)
    _index_rust(forge_bin_dir, host, vault, db_rs)

    assert (
        n_files_py == 18
    )  # 10 host notes + 2 task docs (audit excluded) + 6 vault notes (4+1+1)
    fts_py = _notes_fts_rows(db_py)
    fts_rs = _notes_fts_rows(db_rs)
    assert len(fts_py) == n_files_py
    assert len(fts_rs) == len(fts_py)

    for (path_py, source_py, title_py, body_py, mtime_py), (
        path_rs,
        source_rs,
        title_rs,
        body_rs,
        mtime_rs,
    ) in zip(fts_py, fts_rs):
        assert (path_py, source_py, title_py, body_py) == (
            path_rs,
            source_rs,
            title_rs,
            body_rs,
        )
        assert isinstance(mtime_py, float)
        assert isinstance(mtime_rs, float)
        assert abs(mtime_py - mtime_rs) < 1e-6, (path_py, mtime_py, mtime_rs)

    # excluded tasks/audit/ note must appear in neither database
    assert not any(p.startswith("tasks/audit/") for p, *_ in fts_py)
    assert not any(p.startswith("tasks/audit/") for p, *_ in fts_rs)

    tables_py = _note_tables_rows(db_py)
    tables_rs = _note_tables_rows(db_rs)
    assert tables_py == tables_rs
    assert len(tables_py) == 2  # note_titled.md + note_untitled.md, one table each


@pytest.mark.parametrize("query", ["apple", "banana", "Titled Heading"])
def test_python_and_forge_search_agree(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, forge_bin_dir: str, query: str
) -> None:
    host, vault = _build_fixture(tmp_path)
    db_py = tmp_path / "py.db"
    db_rs = tmp_path / "rs.db"
    _index_python(host, vault, db_py, monkeypatch)
    _index_rust(forge_bin_dir, host, vault, db_rs)

    conn = sqlite3.connect(str(db_py))
    try:
        py_hits = index_notes.search_notes(conn, query)
    finally:
        conn.close()
    py_paths = [h["path"] for h in py_hits]

    forge = str(Path(forge_bin_dir) / "forge")
    result = subprocess.run(
        [
            forge,
            "notes",
            "search",
            "--host",
            str(host),
            "--db",
            str(db_rs),
            "--json",
            query,
        ],
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert result.returncode == 0, f"forge notes search failed: {result.stderr}"
    rs_paths = [hit["path"] for hit in json.loads(result.stdout)]

    assert py_paths == rs_paths, (query, py_paths, rs_paths)
    assert py_paths, (
        f"query {query!r} matched nothing in either index -- fixture is wrong"
    )
