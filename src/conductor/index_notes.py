"""Index research notes + task docs into runs.db for durable, fast search.

Two complementary indexes, both queryable straight from `research/runs.db`:

  1. notes_fts   — SQLite FTS5 full-text index over every .md (prose search).
  2. note_tables — every markdown table extracted as structured rows
                   (headers_json + rows_json), so result matrices buried in
                   notes (e.g. cross_axis_architecture_matrix_2026-06-07.md)
                   stay queryable after the prose is forgotten.

FTS5 ships with Python's stdlib sqlite3 — nothing to install.

Sources: research/notes/**.md (source='notes') and tasks/**.md
(source='tasks') always, plus the Obsidian vault's research/dashboards/
runbooks trees (source='vault_research'/'vault_dashboards'/'vault_runbooks')
additionally when the vault is present on this machine -- the vault never
replaces the repo's own notes, since the two trees hold different notes and
neither is a superset of the other. Excludes generated audit outputs under
tasks/audit/. Idempotent full rebuild each run.

Usage:
  python -m conductor.index_notes                 # rebuild both indexes
  python -m conductor.index_notes search "binding wall semiring"
  python -m conductor.index_notes tables "cross_axis"   # list tables in matching notes
"""

from __future__ import annotations

import glob
import json
import os
import re
import sqlite3
import sys
import time

from conductor.project_paths import host_root, notes_root

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DB_PATH = os.path.join(REPO, "research", "runs.db")
VAULT_ROOT = os.path.expanduser("~/Documents/CodexVault")

VAULT_SOURCES = (
    ("vault_research", os.path.join(VAULT_ROOT, "research")),
    ("vault_dashboards", os.path.join(VAULT_ROOT, "dashboards")),
    ("vault_runbooks", os.path.join(VAULT_ROOT, "runbooks")),
)
TASKS_SOURCE = ("tasks", os.path.join(REPO, "tasks"))


def _fallback_notes_source() -> tuple[str, str]:
    """('notes', the configured notes tree of the workspace this runs in)."""
    return ("notes", str(notes_root(host_root())))


EXCLUDED_REL_PREFIXES = ("tasks/audit/",)

_SEP_RE = re.compile(r"^\s*\|?[\s:|-]*-[\s:|-]*\|?\s*$")
_HEADING_RE = re.compile(r"^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$")
_SEARCH_TERM_RE = re.compile(r"\S+")


def _clean_cell(cell: str) -> str:
    return cell.strip().strip("`").replace("**", "").strip()


def _split_row(line: str) -> list[str]:
    s = line.strip()
    if s.startswith("|"):
        s = s[1:]
    if s.endswith("|"):
        s = s[:-1]
    return [_clean_cell(c) for c in s.split("|")]


def _is_table_row(line: str) -> bool:
    return line.lstrip().startswith("|")


def extract_tables(text: str) -> list[dict]:
    """Return list of {section, table_idx, headers, rows} markdown tables."""
    lines = text.splitlines()
    tables: list[dict] = []
    section = ""
    i = 0
    tidx = 0
    while i < len(lines):
        m = _HEADING_RE.match(lines[i])
        if m:
            section = m.group(2).strip()
            i += 1
            continue
        # A markdown table = header row, separator row, then >=0 data rows.
        if (
            _is_table_row(lines[i])
            and i + 1 < len(lines)
            and _SEP_RE.match(lines[i + 1])
            and "-" in lines[i + 1]
        ):
            headers = _split_row(lines[i])
            j = i + 2
            rows: list[list[str]] = []
            while j < len(lines) and _is_table_row(lines[j]):
                rows.append(_split_row(lines[j]))
                j += 1
            tables.append(
                {
                    "section": section,
                    "table_idx": tidx,
                    "headers": headers,
                    "rows": rows,
                }
            )
            tidx += 1
            i = j
            continue
        i += 1
    return tables


def _title_of(text: str, path: str) -> str:
    for line in text.splitlines():
        m = _HEADING_RE.match(line)
        if m:
            return m.group(2).strip()
    return os.path.basename(path)


def _should_index_path(rel_path: str) -> bool:
    normalized = rel_path.replace(os.sep, "/")
    return not normalized.startswith(EXCLUDED_REL_PREFIXES)


def _source_roots() -> tuple[tuple[str, str], ...]:
    """The repo's own notes + tasks always, the vault trees additionally.

    The vault and the repo's ``research/notes`` hold different notes -- one
    is never a superset of the other -- so a vault present on this machine
    must never suppress the repo's own tree from being indexed.
    """
    roots = (_fallback_notes_source(), TASKS_SOURCE)
    vault_research = os.path.join(VAULT_ROOT, "research")
    if os.path.isdir(vault_research):
        roots = (*roots, *VAULT_SOURCES)
    return roots


def _fts_match_query(query: str) -> str:
    terms = []
    for raw_term in _SEARCH_TERM_RE.findall(query):
        term = raw_term.strip('"')
        if term:
            terms.append(f'"{term.replace(chr(34), chr(34) * 2)}"')
    return " ".join(terms)


def search_notes(
    conn: sqlite3.Connection,
    query: str,
    *,
    limit: int = 20,
    source: str | None = None,
) -> list[dict[str, str]]:
    """Return bounded FTS previews for a query, optionally by source."""

    if limit < 1:
        raise ValueError("limit must be positive")
    match_query = _fts_match_query(query)
    if not match_query:
        return []
    sql = """SELECT path, title,
                     snippet(notes_fts, 3, '[', ']', ' … ', 12) AS snip
                FROM notes_fts
               WHERE notes_fts MATCH ?"""
    params: list[object] = [match_query]
    if source is not None:
        sql += " AND source = ?"
        params.append(source)
    sql += " ORDER BY rank LIMIT ?"
    params.append(limit)
    return [
        {"path": path, "title": title, "snippet": snip}
        for path, title, snip in conn.execute(sql, params).fetchall()
    ]


def _ddl(conn: sqlite3.Connection) -> None:
    conn.executescript(
        """
        CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
            path UNINDEXED, source UNINDEXED, title, body, mtime UNINDEXED
        );
        CREATE TABLE IF NOT EXISTS note_tables (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source TEXT NOT NULL,
            path TEXT NOT NULL,
            note TEXT NOT NULL,
            table_idx INTEGER NOT NULL,
            section_heading TEXT,
            n_cols INTEGER,
            n_rows INTEGER,
            headers_json TEXT NOT NULL,
            rows_json TEXT NOT NULL,
            ingested_at REAL NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_note_tables_note ON note_tables(note);
        """
    )


def rebuild(conn: sqlite3.Connection) -> tuple[int, int]:
    _ddl(conn)
    now = time.time()
    conn.execute("DELETE FROM notes_fts")
    conn.execute("DELETE FROM note_tables")
    n_files = 0
    n_tables = 0
    for source, root in _source_roots():
        if not os.path.isdir(root):
            continue
        for path in sorted(glob.glob(os.path.join(root, "**", "*.md"), recursive=True)):
            rel = (
                os.path.relpath(path, REPO)
                if path.startswith(REPO)
                else os.path.relpath(path, VAULT_ROOT)
            )
            if not _should_index_path(rel):
                continue
            with open(path, errors="ignore") as fh:
                text = fh.read()
            note = os.path.basename(path)
            mtime = os.path.getmtime(path)
            conn.execute(
                "INSERT INTO notes_fts (path, source, title, body, mtime) VALUES (?,?,?,?,?)",
                (rel, source, _title_of(text, path), text, mtime),
            )
            n_files += 1
            for t in extract_tables(text):
                conn.execute(
                    """INSERT INTO note_tables
                       (source, path, note, table_idx, section_heading, n_cols,
                        n_rows, headers_json, rows_json, ingested_at)
                       VALUES (?,?,?,?,?,?,?,?,?,?)""",
                    (
                        source,
                        rel,
                        note,
                        t["table_idx"],
                        t["section"],
                        len(t["headers"]),
                        len(t["rows"]),
                        json.dumps(t["headers"]),
                        json.dumps(t["rows"]),
                        now,
                    ),
                )
                n_tables += 1
    conn.commit()
    return n_files, n_tables


def cmd_search(conn: sqlite3.Connection, query: str) -> None:
    if not _fts_match_query(query):
        print("empty search query")
        return
    rows = search_notes(conn, query)
    if not rows:
        print(f"no matches for: {query}")
        return
    for row in rows:
        print(f"\n# {row['title']}\n  {row['path']}\n  {row['snippet']}")


def cmd_tables(conn: sqlite3.Connection, note_like: str) -> None:
    cur = conn.execute(
        """SELECT note, table_idx, section_heading, n_cols, n_rows, headers_json
           FROM note_tables WHERE note LIKE ? ORDER BY note, table_idx""",
        (f"%{note_like}%",),
    )
    for note, idx, section, ncol, nrow, headers in cur.fetchall():
        hdr = ", ".join(json.loads(headers))
        print(f"{note} [#{idx}] ({nrow}x{ncol}) {section!r}\n    cols: {hdr}")


def main(argv: list[str]) -> None:
    if not os.path.exists(DB_PATH):
        raise FileNotFoundError(f"runs.db not found at {DB_PATH}")
    conn = sqlite3.connect(DB_PATH)
    try:
        if len(argv) >= 2 and argv[1] == "search":
            cmd_search(conn, " ".join(argv[2:]))
        elif len(argv) >= 2 and argv[1] == "tables":
            cmd_tables(conn, " ".join(argv[2:]))
        else:
            n_files, n_tables = rebuild(conn)
            print(f"indexed {n_files} notes, {n_tables} tables into runs.db")
            print("  search: python -m conductor.index_notes search '<query>'")
            print("  tables: python -m conductor.index_notes tables '<note>'")
    finally:
        conn.close()


if __name__ == "__main__":
    main(sys.argv)
