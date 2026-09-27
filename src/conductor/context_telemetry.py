"""Record provider-neutral context-size telemetry without tool contents.

Two things dominate hook cost that used to be invisible here: how long each
hook took (``runner.dispatch`` already measures ``elapsed_ms`` per hook, shown
only under ``HOOK_DISPATCH_TRACE``) and how often the *same* injected context
(SessionStart/compaction ``additionalContext``, tagged ``category="instructions"``
by its caller) gets resent whole. Both are recorded here now, alongside the
existing byte counts, and both feed ``summarize``.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Final

from conductor.project_paths import host_root

ROOT: Final[Path] = host_root()
# The default sink lives under the ledger root (``LEDGER_ROOT`` env var,
# else /mnt/data/llm/ledger), never inside the checkout: the old
# ``<ROOT>/src/research/tmp/...`` default was an inherited defect from the
# native port -- a read-only checkout must not be a hook's write target,
# and ``forge ledger rollup`` reads the telemetry that lands here from the
# same root (``hook_rollup``). Resolved at import time, which for this
# module is per hook invocation (one process per dispatch), so it matches
# the Rust twin's call-time ``telemetry_path()``. ``CONTEXT_TELEMETRY_PATH``
# still overrides the default per call wherever it is honoured.
DEFAULT_PATH: Final[Path] = (
    Path(os.environ.get("LEDGER_ROOT", "/mnt/data/llm/ledger"))
    / "telemetry"
    / "context_telemetry"
    / "events.jsonl"
)
MAX_LOG_BYTES: Final[int] = 10 * 1024 * 1024
MAX_ROTATED_LOGS: Final[int] = 5

# Set once a write fails with an OSError (unwritable directory, disk full): the
# rest of this process stops trying, after one stderr line and a best-effort
# ``telemetry_disabled`` event. Each hook invocation is its own process, so this
# never survives longer than one hook's dispatch -- see ``_mark_disabled``.
_disabled: bool = False


def event(payload: Any) -> dict[str, Any]:
    """Reduce one hook payload to counts and non-sensitive routing labels."""

    from conductor._native import context_telemetry_event_native

    item = context_telemetry_event_native(
        payload,
        datetime.now(UTC).isoformat(timespec="milliseconds"),
    )
    if isinstance(payload, dict):
        session_id = payload.get("session_id")
        if isinstance(session_id, str) and session_id:
            item["session_id"] = session_id
    return item


def _encoded_record(record: dict[str, Any]) -> bytes:
    return (
        json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n"
    ).encode("utf-8")


def _append_encoded(path: Path, encoded: bytes) -> bool:
    from conductor._native import context_telemetry_append_native

    return context_telemetry_append_native(str(path), encoded, MAX_LOG_BYTES)


def append(record: dict[str, Any], path: Path = DEFAULT_PATH) -> bool:
    """Append one record while stopping cleanly at the bounded log budget."""

    return _append_encoded(path, _encoded_record(record))


def _rotated_name(path: Path, stamp: str) -> Path:
    """``events.jsonl`` rotated at *stamp* becomes ``events.<stamp>.jsonl``:
    sortable by name, and a glob on the live file's stem finds every rotation."""

    suffix = path.suffix
    stem = path.name[: -len(suffix)] if suffix else path.name
    return path.with_name(f"{stem}.{stamp}{suffix}" if suffix else f"{stem}.{stamp}")


def _prune_rotated(path: Path, keep: int) -> None:
    """Delete rotated logs beyond the newest *keep*, oldest by modification time
    first: rotation must not accumulate files forever, and sorting by mtime
    (rather than name) stays correct even across a same-second collision broken
    by ``_unique_target``."""

    if keep < 0:
        return
    suffix = path.suffix
    stem = path.name[: -len(suffix)] if suffix else path.name
    pattern = f"{stem}.*{suffix}" if suffix else f"{stem}.*"
    rotated = sorted(
        (candidate for candidate in path.parent.glob(pattern) if candidate != path),
        key=lambda candidate: candidate.stat().st_mtime_ns,
    )
    for stale in rotated[: len(rotated) - keep]:
        stale.unlink(missing_ok=True)


def _utc_stamp() -> str:
    return datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")


def _unique_target(target: Path) -> Path:
    """Disambiguate two rotations landing on the same stamp (same wall-clock
    second): a repeat rename must never silently overwrite the first."""

    if not target.exists():
        return target
    suffix = target.suffix
    stem = target.name[: -len(suffix)] if suffix else target.name
    counter = 2
    while True:
        candidate = target.with_name(f"{stem}-{counter}{suffix}")
        if not candidate.exists():
            return candidate
        counter += 1


def rotate(path: Path, *, keep: int = MAX_ROTATED_LOGS) -> Path:
    """Move a full log aside as ``<name>.<utc-stamp>.jsonl``, prune rotations
    beyond *keep*, and return the new (rotated) path."""

    target = _unique_target(_rotated_name(path, _utc_stamp()))
    path.rename(target)
    _prune_rotated(path, keep)
    return target


def _disabled_event(reason: str) -> dict[str, Any]:
    return {
        "timestamp": datetime.now(UTC).isoformat(timespec="milliseconds"),
        "event": "telemetry_disabled",
        "tool": "context_telemetry",
        "output_bytes": 0,
        "reason": reason,
    }


def _mark_disabled(path: Path, exc: Exception) -> None:
    """Warn once, disable further writes for this process, and best-effort log
    why. Never re-raise: a broken telemetry sink must not break hook dispatch,
    and it must never fail silently either -- the stderr line and the
    ``telemetry_disabled`` event (when the directory allows even that) are the
    two required signals."""

    global _disabled
    if _disabled:
        return
    _disabled = True
    print(
        f"context telemetry disabled for this process: {path} unwritable ({exc})",
        file=sys.stderr,
    )
    try:
        _append_encoded(path, _encoded_record(_disabled_event(str(exc))))
    except OSError as event_exc:
        # Best-effort only: the first failure already proved the sink unwritable,
        # so this one is expected, not new information -- still logged, never
        # swallowed silently.
        print(
            f"context telemetry: could not record telemetry_disabled either: {event_exc}",
            file=sys.stderr,
        )


def record(
    record_: dict[str, Any], path: Path, *, keep_rotated: int = MAX_ROTATED_LOGS
) -> None:
    """Append, rotating a full log first: the cap bounds one file, never drops
    events. An unwritable directory disables telemetry for this process instead
    of raising into hook dispatch -- see ``_mark_disabled``."""

    if _disabled:
        return
    encoded = _encoded_record(record_)
    try:
        if _append_encoded(path, encoded):
            return
        rotated = rotate(path, keep=keep_rotated)
        print(f"context telemetry log rotated to {rotated.name}", file=sys.stderr)
        if not _append_encoded(path, encoded):
            raise OSError(f"record exceeds the log budget even after rotation ({path})")
    except OSError as exc:
        _mark_disabled(path, exc)


def _injected_context_text(hook_json: Any) -> str:
    """The same text the byte count measures: additional context, or a deny
    reason when that's all a quiet hook produced."""

    from conductor._native import context_telemetry_injected_context_native

    return context_telemetry_injected_context_native(hook_json)


def hook_context_event(
    hook: str,
    hook_json: Any,
    *,
    event_name: str = "",
    category: str = "",
    session_id: str = "",
) -> dict[str, Any]:
    """Reduce one hook's stdout JSON to the size of the context it injects.

    ``category`` tags events that resend the same context across SessionStart or
    compaction (e.g. ``"instructions"``) so ``summarize`` can count them apart
    from one-off hook messages. A ``content_hash`` of the injected text -- never
    the text itself -- lets it tell an exact repeat from new content.
    """

    from conductor._native import context_telemetry_hook_event_with_hash_native

    item = context_telemetry_hook_event_with_hash_native(
        hook,
        hook_json,
        event_name,
        datetime.now(UTC).isoformat(timespec="milliseconds"),
    )
    if category:
        item["category"] = category
    if session_id:
        item["session_id"] = session_id
    return item


def hook_timing_event(
    hook_event: str,
    name: str,
    elapsed_ms: float,
    status: str,
    *,
    session_id: str = "",
) -> dict[str, Any]:
    """One hook's ``runner.dispatch`` duration -- the cost ``HOOK_DISPATCH_TRACE``
    used to only ever print to a terminal, now billed into the same ledger as
    the byte counts so ``summarize`` can report ms per hook name."""

    item: dict[str, Any] = {
        "timestamp": datetime.now(UTC).isoformat(timespec="milliseconds"),
        "event": "HookTiming",
        "tool": name,
        "hook_event": hook_event,
        "elapsed_ms": round(float(elapsed_ms), 3),
        "status": status,
        "output_bytes": 0,
    }
    if session_id:
        item["session_id"] = session_id
    return item


def summarize(paths: list[Path], *, bound_bytes: int = 8000) -> dict[str, Any]:
    """Per-(event, tool) byte and token totals; ``over_bound`` = raw results the
    post-bash-quiet hook cuts down before the model sees them."""

    from conductor._native import context_telemetry_summarize_native

    return json.loads(
        context_telemetry_summarize_native([str(path) for path in paths], bound_bytes)
    )


def _parse_since(since: str) -> datetime:
    from conductor._native import context_telemetry_parse_since_native

    cutoff = context_telemetry_parse_since_native(
        since, datetime.now(UTC).isoformat()
    )
    return datetime.fromisoformat(cutoff)


def summarize_report(
    paths: list[Path],
    *,
    bound_bytes: int = 8000,
    since: str | None = None,
    top: int = 10,
) -> dict[str, Any]:
    """Aggregate a filtered rich report in the native core."""

    from conductor._native import context_telemetry_report_native

    # Keep the historical files field: a temporary JSONL pathname which is
    # removed before returning. The native scan reads the source logs directly.
    with tempfile.NamedTemporaryFile(
        "w", suffix=".jsonl", delete=False, encoding="utf-8"
    ) as handle:
        tmp_path = Path(handle.name)
    try:
        return json.loads(
            context_telemetry_report_native(
                [str(path) for path in paths],
                bound_bytes,
                since,
                top,
                datetime.now(UTC).isoformat(),
                str(tmp_path),
            )
        )
    finally:
        tmp_path.unlink(missing_ok=True)


def _format_summary(summary: dict[str, Any]) -> str:
    from conductor._native import context_telemetry_format_summary_native

    return context_telemetry_format_summary_native(json.dumps(summary), False)


def _format_rich_summary(report: dict[str, Any]) -> str:
    from conductor._native import context_telemetry_format_summary_native

    return context_telemetry_format_summary_native(json.dumps(report), True)


def _cmd_record(path: Path) -> int:
    try:
        record(event(json.load(sys.stdin)), path)
    except (OSError, TypeError, ValueError) as exc:
        print(f"context telemetry unavailable: {exc}", file=sys.stderr)
    return 0


def _cmd_hook_context(path: Path, hook: str, event_name: str, category: str) -> int:
    """Tee: log the injected-context size, echo the hook JSON byte-for-byte."""

    raw = sys.stdin.buffer.read()
    sys.stdout.buffer.write(raw)
    sys.stdout.flush()
    try:
        item = hook_context_event(
            hook, json.loads(raw), event_name=event_name, category=category
        )
        if item["output_bytes"]:  # a quiet hook injects nothing: no event
            record(item, path)
    except (OSError, TypeError, ValueError) as exc:
        print(f"context telemetry unavailable: {exc}", file=sys.stderr)
    return 0


def main(argv: list[str] | None = None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command")
    sub.add_parser("record", help="(default) log one PostToolUse payload from stdin")
    hook = sub.add_parser(
        "hook-context", help="log a hook's injected context size; passes stdin through"
    )
    hook.add_argument("--hook", required=True)
    hook.add_argument(
        "--event", default="", help="hook event name if the JSON lacks one"
    )
    hook.add_argument(
        "--category",
        default="",
        help="tag for cross-call repeat counting, e.g. 'instructions'",
    )
    summary = sub.add_parser("summary", help="aggregate one or more event logs")
    summary.add_argument("paths", nargs="*", type=Path)
    summary.add_argument("--bound-bytes", type=int, default=8000)
    summary.add_argument("--json", action="store_true")
    rich = sub.add_parser(
        "summarize", help="summary plus per-session, per-hook ms, instructions resends"
    )
    rich.add_argument("paths", nargs="*", type=Path)
    rich.add_argument("--bound-bytes", type=int, default=8000)
    rich.add_argument("--since", default=None, help="e.g. 30m, 2h, 1d")
    rich.add_argument("--top", type=int, default=10)
    rich.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    path = Path(os.environ.get("CONTEXT_TELEMETRY_PATH", DEFAULT_PATH))
    if args.command == "hook-context":
        return _cmd_hook_context(path, args.hook, args.event, args.category)
    if args.command == "summary":
        paths = args.paths or [path]
        result = summarize(paths, bound_bytes=args.bound_bytes)
        print(json.dumps(result, indent=2) if args.json else _format_summary(result))
        return 0
    if args.command == "summarize":
        paths = args.paths or [path]
        result = summarize_report(
            paths, bound_bytes=args.bound_bytes, since=args.since, top=args.top
        )
        print(
            json.dumps(result, indent=2) if args.json else _format_rich_summary(result)
        )
        return 0
    return _cmd_record(path)


if __name__ == "__main__":
    raise SystemExit(main())
