"""Contracts for seeding a worktree's code-review-graph store.

The fixture store mirrors the real ``.code-review-graph/graph.db`` schema idioms
(``nodes``/``edges``/``metadata`` plus an external-content ``nodes_fts`` fts5 index
with its shadow tables) so the prefix rewrite and FTS rebuild are exercised against
the same shapes they meet in production. Only the subprocess call to the
``code-review-graph`` binary is faked; git runs for real.
"""

from __future__ import annotations

import sqlite3
import subprocess
from collections.abc import Iterator
from pathlib import Path

import pytest

from conductor import crg_seed_worktree as seeder

OLD = "/main/root"
NEW = "/wt/root"

NODES_SQL = """
create table nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    qualified_name TEXT NOT NULL UNIQUE,
    file_path TEXT NOT NULL,
    line_start INTEGER,
    signature TEXT,
    extra TEXT DEFAULT '{}',
    updated_at REAL NOT NULL
)
"""
EDGES_SQL = """
create table edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    source_qualified TEXT NOT NULL,
    target_qualified TEXT NOT NULL,
    file_path TEXT NOT NULL,
    line INTEGER DEFAULT 0
)
"""
FTS_SQL = """
create virtual table nodes_fts using fts5(
    name, qualified_name, file_path, signature,
    content='nodes', content_rowid='id'
)
"""


def _populate(conn: sqlite3.Connection) -> None:
    """Prefixed rows, unprefixed rows, and a live FTS index over them."""
    conn.executescript(
        f"{NODES_SQL};{EDGES_SQL};"
        "create table metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);"
        f"{FTS_SQL};"
    )
    conn.executemany(
        "insert into nodes(kind, name, qualified_name, file_path, line_start,"
        " signature, extra, updated_at) values (?,?,?,?,?,?,?,?)",
        [
            (
                "Function",
                "seed",
                f"{OLD}/conductor/crg_seed_worktree.py::seed",
                f"{OLD}/conductor/crg_seed_worktree.py",
                12,
                f"seed(root={OLD}/x)",
                '{"root": "' + OLD + '/conductor"}',
                1.0,
            ),
            ("File", "pure", "pkg.pure", "relative/pure.py", 1, "pure()", "{}", 2.0),
        ],
    )
    conn.executemany(
        "insert into edges(kind, source_qualified, target_qualified, file_path, line)"
        " values (?,?,?,?,?)",
        [
            ("CALLS", f"{OLD}/a.py::f", f"{OLD}/b.py::g", f"{OLD}/a.py", 3),
            ("CALLS", "rel::f", "rel::g", "relative/pure.py", 4),
        ],
    )
    conn.execute("insert into metadata(key, value) values (?, ?)", ("root", f"{OLD}/"))
    conn.execute("insert into nodes_fts(nodes_fts) values('rebuild')")


def build_store(path: Path) -> None:
    """A miniature graph.db carrying the real store's schema idioms."""
    conn = sqlite3.connect(path)
    try:
        _populate(conn)
        conn.commit()
    finally:
        conn.close()


def cells_with(conn: sqlite3.Connection, needle: str) -> list[tuple[str, str, str]]:
    """Every (table, column, value) in a rewritable table containing needle."""
    found = []
    for table in seeder.rewritable_tables(conn):
        cols = seeder.text_columns(conn, table)
        for row in conn.execute(f'select {", ".join(cols)} from "{table}"'):
            for col, value in zip(cols, row):
                if isinstance(value, str) and needle in value:
                    found.append((table, col, value))
    return found


@pytest.fixture()
def store(tmp_path: Path) -> Path:
    db = tmp_path / "graph.db"
    build_store(db)
    return db


@pytest.fixture()
def conn(store: Path) -> Iterator[sqlite3.Connection]:
    connection = sqlite3.connect(store)
    try:
        yield connection
    finally:
        connection.close()


def test_rewrite_replaces_the_prefix_in_every_text_column_and_spares_other_rows(
    conn: sqlite3.Connection,
) -> None:
    before = {
        (t, c, v) for t, c, v in cells_with(conn, "relative/pure.py") if OLD not in v
    }
    prefixed_columns = {(t, c) for t, c, _ in cells_with(conn, f"{OLD}/")}
    assert prefixed_columns == {
        ("edges", "file_path"),
        ("edges", "source_qualified"),
        ("edges", "target_qualified"),
        ("metadata", "value"),
        ("nodes", "extra"),
        ("nodes", "file_path"),
        ("nodes", "qualified_name"),
        ("nodes", "signature"),
    }
    cells = seeder.rewrite_root_prefix(conn, f"{OLD}/", f"{NEW}/")
    conn.commit()
    assert cells > 0
    assert cells_with(conn, f"{OLD}/") == []
    assert {(t, c) for t, c, _ in cells_with(conn, f"{NEW}/")} == prefixed_columns
    after = {
        (t, c, v) for t, c, v in cells_with(conn, "relative/pure.py") if NEW not in v
    }
    assert after == before


def test_rewrite_skips_the_fts_virtual_table_and_its_shadow_tables(
    conn: sqlite3.Connection,
) -> None:
    names = {
        row[0]
        for row in conn.execute("select name from sqlite_master where type='table'")
    }
    assert {"nodes_fts", "nodes_fts_data", "nodes_fts_idx"} <= names
    assert seeder.fts_table_names(conn) >= {"nodes_fts", "nodes_fts_data"}
    assert set(seeder.rewritable_tables(conn)) == {"nodes", "edges", "metadata"}


def test_fts_rebuild_reindexes_the_rewritten_rows(conn: sqlite3.Connection) -> None:
    seeder.rewrite_root_prefix(conn, f"{OLD}/", f"{NEW}/")
    conn.commit()
    stale = conn.execute(
        "select count(*) from nodes_fts where nodes_fts match ?", ('"wt/root"',)
    ).fetchone()[0]
    assert stale == 0
    assert seeder.rebuild_fts(conn) == ["nodes_fts"]
    conn.commit()
    fresh = conn.execute(
        "select count(*) from nodes_fts where nodes_fts match ?", ('"wt/root"',)
    ).fetchone()[0]
    assert fresh == 1


def test_copy_refuses_while_a_wal_sits_beside_the_source(tmp_path: Path) -> None:
    src = tmp_path / "graph.db"
    build_store(src)
    src.with_name("graph.db-wal").write_bytes(b"")
    dst = tmp_path / "wt" / ".code-review-graph" / "graph.db"
    with pytest.raises(seeder.SeedError, match="write is in flight"):
        seeder.copy_store(src, dst)
    assert not dst.exists()
    seeder.copy_store(src, dst, force=True)
    assert dst.read_bytes() == src.read_bytes()


def test_resolve_crg_bin_prefers_the_env_override_and_fails_loud_when_absent(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(seeder.shutil, "which", lambda _name: None)
    monkeypatch.delenv("CRG_BIN", raising=False)
    with pytest.raises(seeder.SeedError, match="not on PATH"):
        seeder.resolve_crg_bin()
    fake = tmp_path / "code-review-graph"
    fake.write_text("#!/bin/sh\n")
    monkeypatch.setenv("CRG_BIN", str(fake))
    assert seeder.resolve_crg_bin() == str(fake)
    with pytest.raises(seeder.SeedError, match="not found"):
        seeder.resolve_crg_bin(str(tmp_path / "absent" / "code-review-graph"))


def make_worktree(tmp_path: Path) -> Path:
    """A real single-commit git worktree so git_head() runs unmocked."""
    wt = tmp_path / "wt"
    wt.mkdir()
    (wt / "a.py").write_text("x = 1\n")
    for args in (
        ["init", "-q", "-b", "main"],
        ["config", "user.email", "t@example.com"],
        ["config", "user.name", "t"],
        ["add", "a.py"],
        ["commit", "-qm", "seed"],
    ):
        subprocess.run(["git", *args], cwd=wt, check=True, capture_output=True)
    return wt


def fake_crg(monkeypatch: pytest.MonkeyPatch, crg_bin: str, stamp: str | None) -> None:
    """Intercept only the code-review-graph subprocess; git still runs for real."""
    real_run = subprocess.run

    def fake(cmd, *args, **kwargs):
        if list(cmd)[:1] != [crg_bin]:
            return real_run(cmd, *args, **kwargs)
        if stamp is not None:
            db = Path(cmd[cmd.index("--repo") + 1]) / ".code-review-graph" / "graph.db"
            seeder.stamp_head(db, stamp)
        return subprocess.CompletedProcess(cmd, 0, "", "")

    monkeypatch.setattr(seeder.subprocess, "run", fake)


def test_seed_stamps_the_head_when_update_left_it_at_the_main_checkouts_sha(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    main = tmp_path / "main"
    (main / ".code-review-graph").mkdir(parents=True)
    build_store(main / ".code-review-graph" / "graph.db")
    wt = make_worktree(tmp_path)
    fake_crg(monkeypatch, "/fake/crg", stamp="0" * 40)
    report = seeder.seed(main, wt, "/fake/crg")
    head = seeder.git_head(wt)
    assert report["stamped"] is True
    assert report["graph_head_after_update"] == "0" * 40
    assert seeder.read_head(wt / ".code-review-graph" / "graph.db") == head


def test_seed_reports_no_stamp_when_update_already_matched_the_worktree_head(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    main = tmp_path / "main"
    (main / ".code-review-graph").mkdir(parents=True)
    build_store(main / ".code-review-graph" / "graph.db")
    wt = make_worktree(tmp_path)
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=wt, capture_output=True, text=True, check=True
    ).stdout.strip()
    fake_crg(monkeypatch, "/fake/crg", stamp=head)
    report = seeder.seed(main, wt, "/fake/crg")
    assert report["stamped"] is False
    assert report["git_head"] == head
    assert report["fts_rebuilt"] == ["nodes_fts"]
