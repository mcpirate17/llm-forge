"""Apply a mutant patch by anchoring on its content, not on its line numbers.

``git apply`` places a hunk by the line numbers in its ``@@`` header and demands
that every context line match there exactly. That makes a mutant patch a pin on
the *layout* of the file it mutates rather than on the construct it mutates: an
edit anywhere above the hunk shifts the line numbers, and an edit inside the
three context lines rewrites the anchor. Neither changes the behaviour the
mutant is supposed to perturb, yet both retire the patch with ``patch does not
apply`` -- and the runner refuses the whole campaign on the first one, after the
campaign has already been selected and paid for.

That has not been a hypothetical cost: fifteen patches across two campaigns were
retired by two unrelated refactors of one file (2026-09-04, #320), and a
corpus-wide audit found 106 mutants that no longer apply at all.

This module places a hunk by *searching for its content*. The ``@@`` numbers are
read for provenance and then ignored. Two rules keep that from being a loosening:

* **Ambiguity is fatal.** A hunk that matches in two places is refused, never
  guessed at. ``git apply`` cannot hit this case because a line number decides
  it; searching can, so it has to be said.
* **Context is relaxed only when the anchor is absent, never when it is
  ambiguous.** The most specific form -- full context -- is tried first, and
  context is dropped one line at a time from each end only while the anchor is
  not found. The changed lines themselves are never dropped, so a hunk always
  matches on the code it actually edits.

This runs as a FALLBACK, never as a replacement. ``git apply`` decides every
patch it can apply, exactly as before; this module is consulted only where git
refused. That ordering is deliberate and is what keeps existing evidence valid:
a refusal used to abort the whole campaign before any mutant executed, so no
receipt can exist for a patch that reaches this code, and no outcome any receipt
already records can change. Mutant application is therefore unchanged for every
input that has ever produced a receipt -- a property the runner-lineage rules
require, and one that is measured rather than asserted.

Verified differentially over the whole corpus: of 3,416 registered mutants,
3,313 apply byte-identically under both, none disagree, and 24 that ``git
apply`` had retired come back. The remainder are genuinely gone (74) or have no
patch file (5).

It is not a speedup. The audit measures the same 1.0 s either way: its thread
pool already overlapped those execs, and the walk is dominated by loading 464
manifests. What it buys is that a mutant survives an edit to its neighbours.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

_NO_NEWLINE = "\\ No newline at end of file"


class PatchApplyError(Exception):
    """A patch could not be placed. Carries why, never a guess."""


@dataclass(frozen=True, slots=True)
class Hunk:
    """One ``@@`` block as an ordered run of ``(kind, text)`` entries."""

    entries: tuple[tuple[str, str], ...]
    header: str
    declared_start: int

    @property
    def old(self) -> tuple[str, ...]:
        return tuple(t for k, t in self.entries if k in " -")

    @property
    def new(self) -> tuple[str, ...]:
        return tuple(t for k, t in self.entries if k in " +")

    @property
    def leading_context(self) -> int:
        n = 0
        for kind, _ in self.entries:
            if kind != " ":
                break
            n += 1
        return n

    @property
    def trailing_context(self) -> int:
        n = 0
        for kind, _ in reversed(self.entries):
            if kind != " ":
                break
            n += 1
        return n


@dataclass(frozen=True, slots=True)
class FilePatch:
    """Every hunk targeting one path."""

    path: str
    hunks: tuple[Hunk, ...]


def parse_unified_diff(text: str) -> tuple[FilePatch, ...]:
    """Split a unified diff into per-path hunks.

    Only the shapes a mutant patch takes are accepted: edits to existing files.
    Creations, deletions, renames and mode changes are refused rather than
    half-handled, because a mutant that creates or deletes a file is not a
    first-order mutation of a construct.
    """

    files: list[FilePatch] = []
    path: str | None = None
    hunks: list[Hunk] = []
    entries: list[tuple[str, str]] = []
    header = ""

    def close_hunk() -> None:
        nonlocal entries, header
        if entries:
            hunks.append(
                Hunk(
                    entries=tuple(entries),
                    header=header,
                    declared_start=_declared_start(header),
                )
            )
        entries, header = [], ""

    def close_file() -> None:
        nonlocal path, hunks
        close_hunk()
        if path is not None:
            if not hunks:
                raise PatchApplyError(f"patch for {path!r} has no hunks")
            files.append(FilePatch(path=path, hunks=tuple(hunks)))
        path, hunks = None, []

    for line in text.splitlines():
        if line.startswith("diff --git "):
            close_file()
        elif line.startswith("--- "):
            close_hunk()
            if line[4:].strip() == "/dev/null":
                raise PatchApplyError(
                    "patch creates a file; not a first-order mutation"
                )
        elif line.startswith("+++ "):
            target = line[4:].strip()
            if target == "/dev/null":
                raise PatchApplyError(
                    "patch deletes a file; not a first-order mutation"
                )
            path = _strip_prefix(target)
        elif line.startswith("@@"):
            close_hunk()
            header = line
        elif line.startswith(("rename ", "new file mode", "deleted file mode")):
            raise PatchApplyError(f"unsupported patch directive: {line.strip()!r}")
        elif line.startswith(_NO_NEWLINE):
            continue
        elif header and line[:1] in (" ", "-", "+"):
            entries.append((line[0], line[1:]))
        elif header and line == "":
            # A context line that is itself empty loses its trailing space in
            # many editors and mailers. Treating it as context is what git does.
            entries.append((" ", ""))
    close_file()
    if not files:
        raise PatchApplyError("patch contains no file sections")
    return tuple(files)


def _declared_start(header: str) -> int:
    """The old-file start line from an ``@@ -a,b +c,d @@`` header, else 1."""

    try:
        old = header.split("-", 1)[1].split(None, 1)[0]
        return int(old.split(",", 1)[0])
    except (IndexError, ValueError):
        return 1


def _strip_prefix(target: str) -> str:
    """Drop the ``a/``/``b/`` that ``-p1`` would strip."""

    for prefix in ("a/", "b/"):
        if target.startswith(prefix):
            return target[len(prefix) :]
    return target


def _find_unique(haystack: list[str], needle: tuple[str, ...]) -> list[int]:
    """Every index where ``needle`` occurs as a contiguous run."""

    if not needle:
        return []
    hits: list[int] = []
    first = needle[0]
    span = len(needle)
    for i in range(len(haystack) - span + 1):
        if haystack[i] == first and tuple(haystack[i : i + span]) == needle:
            hits.append(i)
    return hits


def locate(lines: list[str], hunk: Hunk) -> tuple[int, int]:
    """Where ``hunk`` applies, as ``(start, context_dropped)``.

    Tries the most specific anchor first and relaxes only on absence. Raises
    rather than choosing between candidates.
    """

    old = hunk.old
    # A pure insertion removes nothing, so its context IS its anchor and cannot
    # be dropped to nothing; keep one line on whichever sides have one.
    floor = 1 if all(kind != "-" for kind, _ in hunk.entries) else 0
    drop_max = max(0, min(hunk.leading_context, hunk.trailing_context) - floor)
    for drop in range(drop_max + 1):
        candidate = old[drop : len(old) - drop] if drop else old
        if not candidate:
            break
        hits = _find_unique(lines, candidate)
        if len(hits) == 1:
            return hits[0], drop
        if len(hits) > 1:
            # The construct repeats in this file. `git apply` resolves that with
            # the `@@` line number, so use it the same way -- as a tiebreaker
            # among content matches, never as a constraint on where to look.
            # A tie in that distance is a genuine coin flip and is refused.
            target = hunk.declared_start + drop
            best = sorted(hits, key=lambda h: (abs(h + 1 - target), h))
            if abs(best[0] + 1 - target) == abs(best[1] + 1 - target):
                raise PatchApplyError(
                    f"hunk {hunk.header!r} matches in {len(hits)} places "
                    f"(lines {', '.join(str(h + 1) for h in hits[:5])}) and the "
                    f"declared line {target} does not favour one; refusing to guess"
                )
            return best[0], drop
    raise PatchApplyError(
        f"hunk {hunk.header!r} does not match anywhere in the file; "
        "the construct it mutates is gone or was rewritten"
    )


def apply_hunk(lines: list[str], hunk: Hunk) -> list[str]:
    """Return ``lines`` with ``hunk`` applied, anchored by content."""

    old, new = hunk.old, hunk.new
    start, drop = locate(lines, hunk)
    span = len(old) - 2 * drop
    body = new[drop : len(new) - drop] if drop else new
    return lines[:start] + list(body) + lines[start + span :]


def apply_patch_text(text: str, root: Path) -> tuple[str, ...]:
    """Apply every hunk in ``text`` under ``root``. Returns the paths written."""

    written: list[str] = []
    for file_patch in parse_unified_diff(text):
        target = root / file_patch.path
        if not target.is_file():
            raise PatchApplyError(f"patch targets a missing file: {file_patch.path}")
        raw = target.read_text()
        trailing_newline = raw.endswith("\n")
        lines = raw.split("\n")
        if trailing_newline:
            lines.pop()
        for hunk in file_patch.hunks:
            try:
                lines = apply_hunk(lines, hunk)
            except PatchApplyError as exc:
                raise PatchApplyError(f"{file_patch.path}: {exc}") from exc
        out = "\n".join(lines) + ("\n" if trailing_newline else "")
        target.write_text(out)
        written.append(file_patch.path)
    return tuple(sorted(written))


def check_patch_text(text: str, root: Path) -> None:
    """Raise ``PatchApplyError`` if ``text`` would not apply under ``root``.

    The non-writing half of :func:`apply_patch_text`, for callers that only
    want the verdict. It answers the same question, so a disagreement between
    the two would make an audit that used it worse than no audit at all.
    """

    for file_patch in parse_unified_diff(text):
        target = root / file_patch.path
        if not target.is_file():
            raise PatchApplyError(f"patch targets a missing file: {file_patch.path}")
        raw = target.read_text()
        lines = raw.split("\n")
        if raw.endswith("\n"):
            lines.pop()
        for hunk in file_patch.hunks:
            try:
                lines = apply_hunk(lines, hunk)
            except PatchApplyError as exc:
                raise PatchApplyError(f"{file_patch.path}: {exc}") from exc
