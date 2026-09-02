"""Seed a fresh worktree's code-review-graph store from the main checkout's store.

    python -m conductor.crg_seed_worktree <main_root> <worktree> [--crg-bin PATH]
                                          [--force] [--quiet]

code-review-graph >= 2.3.8 stamps the *absolute* root it was built under into every
node, edge and flow row, so a plain ``cp graph.db`` is refused with "built with a
different repository root". This copies the store, rewrites the root prefix in every
TEXT column of every non-FTS table, rebuilds the FTS index, runs an incremental
``code-review-graph update`` in the worktree, and verifies ``metadata.git_head_sha``
matches the worktree HEAD (``update`` skips the stamp when the diff since the last
index holds no source it parses — pure ``.toml``/``.json`` edits — so this stamps it
directly and says so). Without that the gate raises CRITICAL
``graph-evidence-incomplete``.

Measured on the live 762 MB / 45 192-node store (2026-09-02, /home/tim/Projects/LLM
-> /tmp worktree): copy + prefix rewrite of 1 342 649 cells + FTS rebuild 5.9 s,
incremental ``update`` 0.4 s, 6.4 s wall — against tens of minutes for a rebuild
from scratch. The sqlite work stays in Python deliberately: it is a one-off
orchestration of a database engine that does the row work itself in C (three
``UPDATE ... replace()`` statements per table, one FTS ``rebuild``), not a hot loop
over rows.
"""

from __future__ import annotations

import argparse
import os
import shutil
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

STORE_DIRNAME = ".code-review-graph"
DB_NAME = "graph.db"
HEAD_KEY = "git_head_sha"


class SeedError(RuntimeError):
    """A seed step failed; the caller must not treat the store as usable."""


def resolve_crg_bin(override: str | None = None) -> str:
    """Resolve the code-review-graph binary: --crg-bin, then $CRG_BIN, then PATH."""
    for candidate in (override, os.environ.get("CRG_BIN")):
        if candidate:
            resolved = shutil.which(candidate) or (
                candidate if Path(candidate).is_file() else None
            )
            if resolved is None:
                raise SeedError(f"code-review-graph binary not found: {candidate!r}")
            return resolved
    found = shutil.which("code-review-graph")
    if found is None:
        raise SeedError(
            "code-review-graph is not on PATH; pass --crg-bin or set CRG_BIN"
        )
    return found


def fts_table_names(conn: sqlite3.Connection) -> set[str]:
    """Names of every fts5 virtual table plus the shadow tables it owns."""
    rows = conn.execute(
        "select name, coalesce(sql, '') from sqlite_master where type = 'table'"
    ).fetchall()
    bases = {name for name, sql in rows if "using fts5" in sql.lower()}
    owned = set(bases)
    for name, _sql in rows:
        if any(name.startswith(f"{base}_") for base in bases):
            owned.add(name)
    return owned


def rewritable_tables(conn: sqlite3.Connection) -> list[str]:
    """Every table whose TEXT columns carry paths: not FTS, not sqlite bookkeeping."""
    skip = fts_table_names(conn) | {"sqlite_sequence"}
    return [
        row[0]
        for row in conn.execute(
            "select name from sqlite_master where type = 'table' order by name"
        )
        if row[0] not in skip and not row[0].startswith("sqlite_")
    ]


def text_columns(conn: sqlite3.Connection, table: str) -> list[str]:
    """Columns that can hold a path: declared TEXT, or untyped (sqlite affinity NONE)."""
    cols = []
    for name, decl in conn.execute(
        "select name, coalesce(type, '') from pragma_table_info(?)", (table,)
    ):
        decl = decl.upper()
        if decl == "" or "TEXT" in decl or "CHAR" in decl or "CLOB" in decl:
            cols.append(name)
    return cols


def rewrite_root_prefix(conn: sqlite3.Connection, old: str, new: str) -> int:
    """Replace the old root prefix with the new one in every TEXT cell that holds it.

    Returns the number of cells rewritten. Rows without the prefix are never touched.
    """
    total = 0
    for table in rewritable_tables(conn):
        for col in text_columns(conn, table):
            cur = conn.execute(
                f'update "{table}" set "{col}" = replace("{col}", ?, ?)'
                f' where instr("{col}", ?) > 0',
                (old, new, old),
            )
            total += cur.rowcount
    return total


def rebuild_fts(conn: sqlite3.Connection) -> list[str]:
    """Rebuild every fts5 index from its content table. Returns the tables rebuilt."""
    rows = conn.execute(
        "select name, coalesce(sql, '') from sqlite_master where type = 'table'"
    ).fetchall()
    rebuilt = []
    for name, sql in rows:
        if "using fts5" not in sql.lower():
            continue
        # fts5 takes the index name as an identifier, and identifiers cannot be
        # bound as parameters; name comes from sqlite_master, never from input.
        statement = 'insert into "%s"("%s") values(\'rebuild\')' % (name, name)
        conn.execute(statement)
        rebuilt.append(name)
    return sorted(rebuilt)


def copy_store(src: Path, dst: Path, force: bool = False) -> None:
    """Copy the source graph.db, refusing while a write is in flight (WAL present)."""
    if not src.is_file():
        raise SeedError(f"no source store at {src}")
    wal = src.with_name(f"{src.name}-wal")
    if wal.exists() and not force:
        raise SeedError(
            f"{wal} exists: a write is in flight on the source store. "
            "Wait for it to settle, or pass --force to copy anyway."
        )
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(src, dst)


def read_head(db: Path) -> str | None:
    """The graph's recorded HEAD sha, or None when the store has never stamped one."""
    conn = sqlite3.connect(db)
    try:
        row = conn.execute(
            "select value from metadata where key = ?", (HEAD_KEY,)
        ).fetchone()
    finally:
        conn.close()
    return row[0] if row else None


def stamp_head(db: Path, sha: str) -> None:
    """Force metadata.git_head_sha to sha (update skips it on .toml/.json-only diffs)."""
    conn = sqlite3.connect(db)
    try:
        conn.execute(
            "insert into metadata(key, value) values(?, ?) "
            "on conflict(key) do update set value = excluded.value",
            (HEAD_KEY, sha),
        )
        conn.commit()
    finally:
        conn.close()


def git_head(worktree: Path) -> str:
    """The worktree's current HEAD sha."""
    proc = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=worktree,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise SeedError(f"git rev-parse HEAD failed in {worktree}: {proc.stderr}")
    return proc.stdout.strip()


def run_update(crg_bin: str, worktree: Path) -> subprocess.CompletedProcess[str]:
    """Incrementally reindex the seeded store against the worktree's tree."""
    proc = subprocess.run(
        [crg_bin, "update", "--repo", str(worktree), "-q"],
        cwd=worktree,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise SeedError(
            f"code-review-graph update failed ({proc.returncode}): "
            f"{proc.stdout[-400:]}{proc.stderr[-600:]}"
        )
    return proc


def seed(
    main_root: Path,
    worktree: Path,
    crg_bin: str,
    force: bool = False,
) -> dict[str, object]:
    """Copy, rewrite, rebuild, update and head-verify. Returns a report dict."""
    main_root, worktree = main_root.resolve(), worktree.resolve()
    if main_root == worktree:
        raise SeedError("source and destination roots are the same checkout")
    src = main_root / STORE_DIRNAME / DB_NAME
    dst = worktree / STORE_DIRNAME / DB_NAME
    old, new = f"{main_root}/", f"{worktree}/"

    t0 = time.monotonic()
    copy_store(src, dst, force=force)
    conn = sqlite3.connect(dst)
    try:
        cells = rewrite_root_prefix(conn, old, new)
        rebuilt = rebuild_fts(conn)
        conn.commit()
        left = conn.execute(
            "select count(*) from nodes where instr(file_path, ?) > 0", (old,)
        ).fetchone()[0]
    finally:
        conn.close()
    if left:
        raise SeedError(f"{left} node rows still carry the old root prefix {old!r}")
    seed_seconds = time.monotonic() - t0

    t1 = time.monotonic()
    run_update(crg_bin, worktree)
    update_seconds = time.monotonic() - t1

    want = git_head(worktree)
    got = read_head(dst)
    stamped = got != want
    if stamped:
        stamp_head(dst, want)
    return {
        "db": str(dst),
        "cells": cells,
        "fts_rebuilt": rebuilt,
        "seed_seconds": seed_seconds,
        "update_seconds": update_seconds,
        "git_head": want,
        "graph_head_after_update": got,
        "stamped": stamped,
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m conductor.crg_seed_worktree",
        description="Seed a worktree's code-review-graph store from the main store.",
    )
    parser.add_argument("main_root", type=Path, help="the main checkout to copy from")
    parser.add_argument("worktree", type=Path, help="the worktree to seed")
    parser.add_argument(
        "--crg-bin",
        default=None,
        help="code-review-graph binary (default: $CRG_BIN, else PATH lookup)",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="copy even when a graph.db-wal sits beside the source",
    )
    parser.add_argument("--quiet", action="store_true", help="print only the verdict")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        report = seed(
            args.main_root,
            args.worktree,
            resolve_crg_bin(args.crg_bin),
            force=args.force,
        )
    except SeedError as exc:
        print(f"crg-seed | FAIL {exc}", file=sys.stderr)
        return 1
    if not args.quiet:
        print(
            f"rewrote {report['cells']} cells in {report['seed_seconds']:.1f}s; "
            f"fts rebuilt: {', '.join(report['fts_rebuilt']) or 'none'}"
        )
        print(f"update {report['update_seconds']:.1f}s")
        if report["stamped"]:
            print(
                "update skipped the head stamp "
                f"(found {report['graph_head_after_update']}); "
                f"stamped {HEAD_KEY}={report['git_head'][:10]} directly"
            )
    print(
        f"crg-seed | PASS {report['db']} head={report['git_head'][:10]} "
        f"stamped={'yes' if report['stamped'] else 'no'}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
