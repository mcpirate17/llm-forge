"""Read-only adapter for the repository code-review graph."""

from __future__ import annotations

import hashlib
import sqlite3
import subprocess
from dataclasses import dataclass
from pathlib import Path

SUPPORTED_SCHEMA = 9
REQUIRED_TABLES = {"nodes", "edges", "metadata"}


@dataclass(frozen=True, slots=True)
class GraphStatus:
    available: bool
    complete: bool
    reason: str
    graph_head: str = ""
    repo_head: str = ""
    overlay_hash: str = ""
    schema_version: int = 0

    def as_dict(self) -> dict:
        return {
            "available": self.available,
            "complete": self.complete,
            "reason": self.reason,
            "graph_head": self.graph_head,
            "repo_head": self.repo_head,
            "overlay_hash": self.overlay_hash,
            "schema_version": self.schema_version,
        }


@dataclass(frozen=True, slots=True)
class Symbol:
    kind: str
    name: str
    qualified_name: str
    file: str
    line_start: int
    line_end: int
    language: str
    params: str
    is_test: bool

    @property
    def stable_id(self) -> str:
        signature = self.params.strip() or "()"
        return f"{self.language}:{self.file}::{self.name}{signature}"


class GraphIndex:
    """Versioned read-only access to graph nodes and relationships."""

    def __init__(self, repo: Path, path: Path | None = None) -> None:
        self.repo = repo.resolve()
        self.path = path or self.repo / ".code-review-graph" / "graph.db"

    def _connect(self) -> sqlite3.Connection:
        uri = f"file:{self.path}?mode=ro"
        connection = sqlite3.connect(uri, uri=True)
        connection.row_factory = sqlite3.Row
        return connection

    def _repo_head(self) -> str:
        proc = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=self.repo,
            capture_output=True,
            text=True,
            check=False,
        )
        return proc.stdout.strip() if proc.returncode == 0 else ""

    def _overlay(self, targets: list[str]) -> tuple[str, list[str]]:
        proc = subprocess.run(
            ["git", "status", "--porcelain", "-z", "--", *(targets or ["."])],
            cwd=self.repo,
            capture_output=True,
            check=False,
        )
        if proc.returncode != 0:
            return "", []
        paths: list[str] = []
        digest = hashlib.sha256()
        for record in proc.stdout.split(b"\0"):
            if len(record) < 4:
                continue
            raw_path = record[3:].decode("utf-8", "replace")
            path = raw_path.split(" -> ")[-1]
            paths.append(path)
            digest.update(record)
            candidate = self.repo / path
            if candidate.is_file():
                digest.update(candidate.read_bytes())
        return digest.hexdigest() if paths else "clean", sorted(paths)

    def status(self, targets: list[str] | None = None) -> GraphStatus:
        repo_head = self._repo_head()
        if not self.path.exists():
            return GraphStatus(
                False, False, "graph database is missing", repo_head=repo_head
            )
        try:
            with self._connect() as connection:
                tables = {
                    row[0]
                    for row in connection.execute(
                        "SELECT name FROM sqlite_master WHERE type='table'"
                    )
                }
                if not REQUIRED_TABLES <= tables:
                    return GraphStatus(
                        False,
                        False,
                        "graph database lacks required tables",
                        repo_head=repo_head,
                    )
                metadata = dict(connection.execute("SELECT key, value FROM metadata"))
        except sqlite3.Error as exc:
            return GraphStatus(
                False, False, f"graph database error: {exc}", repo_head=repo_head
            )
        schema = int(metadata.get("schema_version", 0))
        graph_head = metadata.get("git_head_sha", "")
        overlay_hash, dirty = self._overlay(targets or [])
        if schema != SUPPORTED_SCHEMA:
            reason = f"unsupported graph schema {schema}; expected {SUPPORTED_SCHEMA}"
            complete = False
        elif not repo_head or graph_head != repo_head:
            reason = "graph commit does not match HEAD"
            complete = False
        elif dirty:
            reason = f"HEAD graph with dirty overlay for {len(dirty)} path(s)"
            complete = False
        else:
            reason = "graph matches clean HEAD"
            complete = True
        return GraphStatus(
            True,
            complete,
            reason,
            graph_head=graph_head,
            repo_head=repo_head,
            overlay_hash=overlay_hash,
            schema_version=schema,
        )

    def symbols(self, *, languages: set[str] | None = None) -> list[Symbol]:
        clauses = ["kind IN ('Function', 'Class', 'Test')"]
        params: list[str] = []
        if languages:
            marks = ",".join("?" for _ in languages)
            clauses.append(f"language IN ({marks})")
            params.extend(sorted(languages))
        query = (
            "SELECT kind,name,qualified_name,file_path,line_start,line_end,language,params,is_test "
            f"FROM nodes WHERE {' AND '.join(clauses)} ORDER BY file_path,line_start,name"
        )
        with self._connect() as connection:
            rows = connection.execute(query, params).fetchall()
        out: list[Symbol] = []
        for row in rows:
            path = Path(row["file_path"])
            try:
                rel = path.relative_to(self.repo).as_posix()
            except ValueError:
                continue
            out.append(
                Symbol(
                    kind=row["kind"],
                    name=row["name"],
                    qualified_name=row["qualified_name"],
                    file=rel,
                    line_start=int(row["line_start"] or 0),
                    line_end=int(row["line_end"] or 0),
                    language=row["language"] or "unknown",
                    params=row["params"] or "",
                    is_test=bool(row["is_test"]),
                )
            )
        return out

    def native_reuse_rows(self) -> list[dict[str, object]]:
        """Return reuse-candidate inputs with caller counts in one graph query."""

        query = """
            SELECT n.name, n.file_path, n.line_start,
                   n.language, n.params, MIN(COALESCE(c.caller_count, 0), 20)
            FROM nodes AS n
            LEFT JOIN (
                SELECT target_qualified, COUNT(DISTINCT source_qualified) AS caller_count
                FROM edges
                WHERE kind IN ('CALLS', 'REFERENCES')
                GROUP BY target_qualified
            ) AS c ON c.target_qualified = n.qualified_name
            WHERE n.kind IN ('Function', 'Class')
              AND COALESCE(n.is_test, 0) = 0
              AND n.language IN ('python', 'c', 'cpp', 'rust')
            ORDER BY n.file_path, n.line_start, n.name
        """
        prefix = f"{self.repo}/"
        with self._connect() as connection:
            rows = connection.execute(query).fetchall()
        output: list[dict[str, object]] = []
        for row in rows:
            absolute = str(row["file_path"])
            if not absolute.startswith(prefix):
                continue
            output.append(
                {
                    "name": row["name"],
                    "file": absolute[len(prefix) :],
                    "line_start": int(row["line_start"] or 0),
                    "language": row["language"] or "unknown",
                    "params": row["params"] or "",
                    "caller_count": int(row[5]),
                }
            )
        return output

    def edge_count(self, kind: str) -> int:
        with self._connect() as connection:
            row = connection.execute(
                "SELECT COUNT(*) FROM edges WHERE kind = ?", (kind,)
            ).fetchone()
        return int(row[0])

    def callers(self, qualified_name: str, *, limit: int = 20) -> list[str]:
        with self._connect() as connection:
            rows = connection.execute(
                "SELECT DISTINCT source_qualified FROM edges "
                "WHERE kind IN ('CALLS','REFERENCES') AND target_qualified = ? "
                "ORDER BY source_qualified LIMIT ?",
                (qualified_name, limit),
            ).fetchall()
        return [row[0] for row in rows]

    def import_edges(self) -> list[tuple[str, str, str, int]]:
        with self._connect() as connection:
            rows = connection.execute(
                "SELECT source_qualified,target_qualified,file_path,line FROM edges "
                "WHERE kind = 'IMPORTS_FROM' ORDER BY source_qualified,target_qualified"
            ).fetchall()
        return [(row[0], row[1], row[2], int(row[3] or 0)) for row in rows]
