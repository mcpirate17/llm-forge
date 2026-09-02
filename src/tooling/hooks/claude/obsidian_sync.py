#!/usr/bin/env python3
"""Mirror Claude Code session learnings into the Obsidian vault.

Two subcommands invoked from .claude/settings.json:

  post-edit    PostToolUse on Edit|Write. If the edited file is a memory
               entry under ~/.claude/projects/.../memory/, mirror it to
               CodexVault/claude/memory/ as a thin backlink note. Always
               log the edit to a per-session accumulator so session-end
               can summarize.

  session-end  SessionEnd. Read the accumulator, append a timestamped
               block to CodexVault/claude/YYYY-MM-DD.md with files
               edited + memory writes + git state, then drop the
               accumulator.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


def repo_root() -> Path:
    """The checkout this hook belongs to.

    The hook protocol guarantees no cwd, and only Claude Code exports
    ``CLAUDE_PROJECT_DIR`` (Codex does not); the exec launcher at the old hook
    path exports ``PROJECT_DIR``. Failing both, the authority is the script's
    own location: hook bodies always sit at ``<root>/tooling/hooks/<vendor>/``,
    which holds inside a linked worktree too. That ancestor is accepted directly
    when it carries a ``.git`` entry -- a stat, because this runs on every
    Edit/Write. ``git rev-parse --show-toplevel`` is the fallback for an
    installed-elsewhere copy, and the layout guess stands if git cannot answer.
    """
    for var in ("CLAUDE_PROJECT_DIR", "PROJECT_DIR"):
        env_root = os.environ.get(var, "").strip()
        if env_root:
            return Path(env_root).expanduser().resolve()
    here = Path(__file__).resolve().parent
    guess = here.parents[2]
    if (guess / ".git").exists():
        return guess
    try:
        out = subprocess.run(  # noqa: S603
            ["git", "-C", str(here), "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        if out.returncode == 0 and out.stdout.strip():
            return Path(out.stdout.strip())
    except (OSError, subprocess.SubprocessError):
        pass
    return guess


def _env_dir(name: str, default: Path, *, root: Path) -> Path:
    """``$name`` as a directory, resolving a relative value against *root*."""
    raw = os.environ.get(name, "").strip()
    if not raw:
        return default
    path = Path(raw).expanduser()
    return path if path.is_absolute() else root / path


def _memory_slug(root: Path) -> str:
    """Claude Code's per-project memory dir name for *root*."""
    return str(root).replace("/", "-").replace("_", "-").replace(".", "-")


REPO_ROOT = repo_root()
_VAULT = _env_dir(
    "OBSIDIAN_VAULT_ROOT", Path.home() / "Documents" / "CodexVault", root=REPO_ROOT
)
VAULT_ROOT = _VAULT / "claude"
RESEARCH_VAULT = _VAULT / "research"
NOTES_SOURCE = _env_dir(
    "WORKSPACE_NOTES_DIR", REPO_ROOT / "research" / "notes", root=REPO_ROOT
)
_ = shutil.copy2  # keep formatter from stripping the import; _sync_notes_dir uses it
ACCUM_DIR = Path("/tmp/claude-session-journal")  # nosec B108 — non-sensitive session journal scratch
MEMORY_ROOT = _env_dir(
    "CLAUDE_MEMORY_ROOT",
    Path.home() / ".claude" / "projects" / _memory_slug(REPO_ROOT) / "memory",
    root=REPO_ROOT,
)
SLUG_RE = re.compile(r"[^a-z0-9_-]+")
FM_LINE_RE = re.compile(r"^([A-Za-z_][\w-]*)\s*:\s*(.+?)\s*$")


def _emit_ok(event: str = "PostToolUse") -> None:
    print(json.dumps({"hookSpecificOutput": {"hookEventName": event}}))


def _slug(s: str) -> str:
    return SLUG_RE.sub("-", s.lower()).strip("-") or "untitled"


def _parse_frontmatter(text: str) -> tuple[dict[str, str], str]:
    if not text.startswith("---\n"):
        return {}, text
    end = text.find("\n---", 4)
    if end == -1:
        return {}, text
    fm: dict[str, str] = {}
    for line in text[4:end].splitlines():
        m = FM_LINE_RE.match(line)
        if m:
            fm[m.group(1)] = m.group(2).strip().strip('"').strip("'")
    body = text[end + 4 :].lstrip("\n")
    return fm, body


def _mirror_memory(memory_path: Path) -> None:
    """Mirror one memory file to the vault as a thin backlink note."""
    if memory_path.name == "MEMORY.md":
        return
    try:
        text = memory_path.read_text()
    except OSError:
        return
    fm, body = _parse_frontmatter(text)
    name = fm.get("name") or memory_path.stem
    description = fm.get("description", "")
    mtype = fm.get("type", "memory")
    today = datetime.now().strftime("%Y-%m-%d")

    out_dir = VAULT_ROOT / "memory"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / f"{_slug(memory_path.stem)}.md"

    parts = [
        "---",
        f"date: {today}",
        "source: claude-code-memory",
        f"type: {mtype}",
        f'canonical: "{memory_path}"',
        f"tags: [project/llm, source/claude-code, memory/{mtype}]",
        "---",
        "",
        f"# {name}",
        "",
        f"> **Canonical** (auto-memory): `{memory_path}`",
        "> Thin mirror — the repo memory file is the source of truth.",
        "",
    ]
    if description:
        parts += [f"**{description}**", ""]
    if body.strip():
        parts.append(body.rstrip())
        parts.append("")
    out_path.write_text("\n".join(parts))


def _is_memory_file(fp: Path) -> bool:
    try:
        return MEMORY_ROOT in fp.parents and fp.suffix == ".md"
    except Exception:
        return False


def cmd_post_edit() -> None:
    try:
        data = json.load(sys.stdin)
    except Exception:
        _emit_ok()
        return
    fp_raw = (data.get("tool_input") or {}).get("file_path", "")
    sid = data.get("session_id") or "unknown"
    if not fp_raw:
        _emit_ok()
        return

    fp = Path(fp_raw)
    ts = datetime.now(timezone.utc).isoformat()
    kind = "edit"

    if _is_memory_file(fp):
        if fp.name == "MEMORY.md":
            kind = "memory-index"
        else:
            kind = "memory"
            try:
                _mirror_memory(fp)
            except Exception:
                pass

    try:
        ACCUM_DIR.mkdir(parents=True, exist_ok=True)
        with (ACCUM_DIR / f"{sid}.tsv").open("a") as f:
            f.write(f"{ts}\t{kind}\t{fp_raw}\n")
    except OSError:
        pass

    _emit_ok()


def _git(cwd: str, *args: str) -> str:
    try:
        out = subprocess.run(
            ["git", "-C", cwd, *args],
            capture_output=True,
            text=True,
            timeout=5,
        )
        return out.stdout.strip()
    except Exception:
        return ""


def _read_accumulator(accum: Path) -> tuple[dict[str, str], dict[str, str]]:
    """Return (edits, memories) keyed by file path → last timestamp."""
    edits: dict[str, str] = {}
    memories: dict[str, str] = {}
    if not accum.exists():
        return edits, memories
    for line in accum.read_text().splitlines():
        parts = line.split("\t", 2)
        if len(parts) != 3:
            continue
        t, kind, fp = parts
        if kind == "memory":
            memories[fp] = t
        elif kind == "edit":
            edits[fp] = t
    return edits, memories


def _daily_header(today: str) -> list[str]:
    return [
        "---",
        f"date: {today}",
        "source: claude-code",
        "tags: [project/llm, source/claude-code, journal/daily]",
        "---",
        "",
        f"# {today} — Claude Code Sessions",
        "",
    ]


def _format_block(
    *,
    now: str,
    sid: str,
    branch: str,
    head: str,
    reason: str,
    memories: dict[str, str],
    edits: dict[str, str],
    status: str,
) -> list[str]:
    lines = [
        f"## {now} — session `{sid[:8]}`",
        f"- Branch: `{branch}`  HEAD: `{head}`",
    ]
    if reason:
        lines.append(f"- End reason: `{reason}`")
    if memories:
        lines += ["", f"### Memory writes ({len(memories)})"]
        for fp in sorted(memories):
            lines.append(f"- [[memory/{_slug(Path(fp).stem)}]] — `{fp}`")
    if edits:
        lines += ["", f"### Files edited ({len(edits)})"]
        for fp in sorted(edits):
            lines.append(f"- `{fp}`")
    if status:
        lines += ["", "### git status at session end", "```", status, "```"]
    lines.append("")
    return lines


def cmd_session_end() -> None:
    try:
        data = json.load(sys.stdin)
    except Exception:
        data = {}
    sid = data.get("session_id") or "unknown"
    cwd = data.get("cwd") or os.getcwd()
    reason = data.get("reason") or ""
    accum = ACCUM_DIR / f"{sid}.tsv"

    edits, memories = _read_accumulator(accum)
    if not edits and not memories and not reason:
        sys.exit(0)

    today = datetime.now().strftime("%Y-%m-%d")
    now = datetime.now().astimezone().isoformat(timespec="seconds")
    VAULT_ROOT.mkdir(parents=True, exist_ok=True)
    daily = VAULT_ROOT / f"{today}.md"

    lines = [] if daily.exists() else _daily_header(today)
    lines += _format_block(
        now=now,
        sid=sid,
        branch=_git(cwd, "branch", "--show-current") or "?",
        head=_git(cwd, "rev-parse", "--short", "HEAD") or "?",
        reason=reason,
        memories=memories,
        edits=edits,
        status=_git(cwd, "status", "--porcelain"),
    )

    with daily.open("a") as f:
        f.write("\n".join(lines))

    try:
        accum.unlink()
    except OSError:
        pass


def _sync_notes_dir(*, dry_run: bool) -> tuple[int, int, int]:
    """Mirror the workspace notes dir's *.md into the vault's notes dir.

    Source is ``$WORKSPACE_NOTES_DIR`` (see module constants); destination is
    ``$OBSIDIAN_VAULT_ROOT/research``. Top-level .md files only — subdirs are
    runtime/archival, not knowledge artifacts. Copies only when the source is
    newer than the target; never deletes target files (additive only).

    Returns (copied, skipped, missing_source).
    """
    if not NOTES_SOURCE.is_dir():
        return (0, 0, 1)
    RESEARCH_VAULT.mkdir(parents=True, exist_ok=True)
    copied = skipped = 0
    for src in sorted(NOTES_SOURCE.glob("*.md")):
        dst = RESEARCH_VAULT / src.name
        if dst.exists() and dst.stat().st_mtime >= src.stat().st_mtime:
            skipped += 1
            continue
        if dry_run:
            print(f"[dry-run] {src} -> {dst}", file=sys.stderr)
        else:
            shutil.copy2(src, dst)
        copied += 1
    return (copied, skipped, 0)


def cmd_sync_notes() -> None:
    dry_run = "--dry-run" in sys.argv[2:]
    copied, skipped, missing = _sync_notes_dir(dry_run=dry_run)
    if missing:
        print(
            f"sync-notes: notes dir missing: {NOTES_SOURCE} "
            "(set WORKSPACE_NOTES_DIR to the workspace notes dir)",
            file=sys.stderr,
        )
        sys.exit(1)
    verb = "would copy" if dry_run else "copied"
    print(
        f"sync-notes: {verb} {copied}, skipped {skipped} (already current)",
        file=sys.stderr,
    )


def main() -> None:
    if len(sys.argv) < 2:
        sys.exit(0)
    sub = sys.argv[1]
    if sub == "post-edit":
        cmd_post_edit()
    elif sub == "session-end":
        cmd_session_end()
    elif sub == "sync-notes":
        cmd_sync_notes()


if __name__ == "__main__":
    main()
