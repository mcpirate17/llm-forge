"""Record provider-neutral context-size telemetry without tool contents.

Two things dominate hook cost that used to be invisible here: how long each
hook took (``runner.dispatch`` already measures ``elapsed_ms`` per hook, shown
only under ``HOOK_DISPATCH_TRACE``) and how often the *same* injected context
(SessionStart/compaction ``additionalContext``, tagged ``category="instructions"``
by its caller) gets resent whole. Both are recorded here now, alongside the
existing byte counts, and both feed ``summarize``.
"""

from __future__ import annotations

from collections import Counter
from datetime import UTC, datetime, timedelta
import hashlib
import json
import os
import re
import sys
import tempfile
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
_SINCE_RE: Final[re.Pattern[str]] = re.compile(r"^(\d+)([smhd])$")
_SINCE_UNIT_SECONDS: Final[dict[str, int]] = {"s": 1, "m": 60, "h": 3600, "d": 86400}

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

    if not isinstance(hook_json, dict):
        return ""
    specific = hook_json.get("hookSpecificOutput")
    if not isinstance(specific, dict):
        return ""
    context = specific.get("additionalContext")
    if isinstance(context, str) and context:
        return context
    reason = specific.get("permissionDecisionReason")
    return reason if isinstance(reason, str) else ""


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

    from conductor._native import context_telemetry_hook_event_native

    item = context_telemetry_hook_event_native(
        hook,
        hook_json,
        event_name,
        datetime.now(UTC).isoformat(timespec="milliseconds"),
    )
    if category:
        item["category"] = category
    if session_id:
        item["session_id"] = session_id
    text = _injected_context_text(hook_json)
    if text:
        item["content_hash"] = hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]
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


def _load_events(paths: list[Path]) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    for path in paths:
        if not path.is_file():
            continue
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                item = json.loads(line)
            except ValueError:
                continue
            if isinstance(item, dict):
                events.append(item)
    return events


def _parse_since(since: str) -> datetime:
    match = _SINCE_RE.fullmatch(since.strip())
    if not match:
        raise ValueError(f"--since must look like 30m, 2h or 1d (got {since!r})")
    amount = int(match.group(1))
    return datetime.now(UTC) - timedelta(
        seconds=amount * _SINCE_UNIT_SECONDS[match.group(2)]
    )


def _event_time(item: dict[str, Any]) -> datetime | None:
    value = item.get("timestamp")
    if not isinstance(value, str):
        return None
    try:
        return datetime.fromisoformat(value)
    except ValueError:
        return None


def _percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    rank = (len(ordered) - 1) * pct
    low, high = int(rank), min(int(rank) + 1, len(ordered) - 1)
    if low == high:
        return ordered[low]
    return ordered[low] + (ordered[high] - ordered[low]) * (rank - low)


def _session_breakdown(events: list[dict[str, Any]]) -> dict[str, dict[str, int]]:
    """Events, bytes and hook-context bytes per ``session_id``; events with no
    session id (most hook-context and hook-timing writers don't carry one yet)
    fall into ``"unknown"`` rather than being silently dropped."""

    sessions: dict[str, dict[str, int]] = {}
    for item in events:
        session_id = item.get("session_id") or "unknown"
        bucket = sessions.setdefault(
            session_id, {"events": 0, "output_bytes": 0, "hook_context_bytes": 0}
        )
        bucket["events"] += 1
        output_bytes = item.get("output_bytes")
        if isinstance(output_bytes, (int, float)):
            bucket["output_bytes"] += int(output_bytes)
            if item.get("event") == "HookContext":
                bucket["hook_context_bytes"] += int(output_bytes)
    return sessions


def _hook_timing_stats(events: list[dict[str, Any]]) -> dict[str, Any]:
    by_hook: dict[str, list[float]] = {}
    for item in events:
        if item.get("event") != "HookTiming":
            continue
        elapsed_ms = item.get("elapsed_ms")
        if isinstance(elapsed_ms, (int, float)):
            by_hook.setdefault(str(item.get("tool", "?")), []).append(float(elapsed_ms))
    by_hook_stats = {
        hook: {
            "count": len(values),
            "total_ms": round(sum(values), 3),
            "p50_ms": round(_percentile(values, 0.5), 3),
            "p90_ms": round(_percentile(values, 0.9), 3),
        }
        for hook, values in by_hook.items()
    }
    total_ms = round(sum(stats["total_ms"] for stats in by_hook_stats.values()), 3)
    return {"total_ms": total_ms, "by_hook": by_hook_stats}


def _instructions_stats(events: list[dict[str, Any]]) -> dict[str, Any]:
    matching = [item for item in events if item.get("category") == "instructions"]
    total_bytes = sum(int(item.get("output_bytes") or 0) for item in matching)
    hashes = [item["content_hash"] for item in matching if item.get("content_hash")]
    counts = Counter(hashes)
    repeats = sum(count - 1 for count in counts.values() if count > 1)
    return {
        "resends": len(matching),
        "bytes": total_bytes,
        "distinct_content": len(counts),
        "repeat_resends": repeats,
    }


def _top_message_templates(
    events: list[dict[str, Any]], *, limit: int
) -> list[dict[str, Any]]:
    """The top HookContext (hook, hook_event) pairs by total bytes injected --
    "message templates" because a given hook's message is structurally the same
    every time (only its embedded counts change), so grouping by hook name and
    event stands in for grouping by message shape without storing any text."""

    totals: dict[tuple[str, str], dict[str, int]] = {}
    for item in events:
        if item.get("event") != "HookContext":
            continue
        key = (str(item.get("tool", "?")), str(item.get("hook_event", "?")))
        bucket = totals.setdefault(key, {"count": 0, "output_bytes": 0})
        bucket["count"] += 1
        bucket["output_bytes"] += int(item.get("output_bytes") or 0)
    ranked = sorted(
        (
            {"hook": hook, "hook_event": hook_event, **stats}
            for (hook, hook_event), stats in totals.items()
        ),
        key=lambda row: row["output_bytes"],
        reverse=True,
    )
    return ranked[:limit]


def summarize_report(
    paths: list[Path],
    *,
    bound_bytes: int = 8000,
    since: str | None = None,
    top: int = 10,
) -> dict[str, Any]:
    """Everything ``summarize`` reports, plus per-session totals, per-hook
    dispatch ms (p50/p90), instructions-resend counts/bytes, and the top message
    templates by bytes. ``--since`` (e.g. ``30m``, ``2h``, ``1d``) filters every
    number in the report, including the legacy byte rows."""

    events = _load_events(paths)
    if since is not None:
        cutoff = _parse_since(since)
        events = [
            item
            for item in events
            if (when := _event_time(item)) is not None and when >= cutoff
        ]
    with tempfile.NamedTemporaryFile(
        "w", suffix=".jsonl", delete=False, encoding="utf-8"
    ) as handle:
        for item in events:
            handle.write(json.dumps(item) + "\n")
        tmp_path = Path(handle.name)
    try:
        report = summarize([tmp_path], bound_bytes=bound_bytes)
    finally:
        tmp_path.unlink(missing_ok=True)
    report["since"] = since or "all-time"
    report["sessions"] = _session_breakdown(events)
    report["hook_ms"] = _hook_timing_stats(events)
    report["instructions"] = _instructions_stats(events)
    report["top_message_templates"] = _top_message_templates(events, limit=top)
    return report


def _format_summary(summary: dict[str, Any]) -> str:
    lines = [
        (
            f"events={summary['events']} output_bytes={summary['output_bytes']:,} "
            f"hook_context_bytes={summary['hook_context_bytes']:,} bound={summary['bound_bytes']}"
        ),
        f"{'event':<14}{'tool':<22}{'count':>7}{'bytes':>13}{'share':>7}{'>bound':>8}{'bytes>bound':>13}",
    ]
    for r in summary["rows"]:
        lines.append(
            f"{r['event']:<14}{r['tool']:<22}{r['count']:>7}{r['output_bytes']:>13,}"
            f"{r['share']:>7.1%}{r['over_bound']:>8}{r['over_bound_bytes']:>13,}"
        )
    return "\n".join(lines)


def _format_rich_summary(report: dict[str, Any]) -> str:
    lines = [_format_summary(report), f"since={report['since']}"]
    lines.append(f"sessions={len(report['sessions'])}")
    for session_id, totals in report["sessions"].items():
        lines.append(
            f"  {session_id:<20} events={totals['events']:>6} "
            f"output_bytes={totals['output_bytes']:>10,} "
            f"hook_context_bytes={totals['hook_context_bytes']:>8,}"
        )
    lines.append(f"hook_ms total={report['hook_ms']['total_ms']:.1f}")
    for hook, stats in sorted(
        report["hook_ms"]["by_hook"].items(), key=lambda kv: -kv[1]["total_ms"]
    ):
        lines.append(
            f"  {hook:<28} count={stats['count']:>5} total_ms={stats['total_ms']:>9.1f} "
            f"p50_ms={stats['p50_ms']:>7.1f} p90_ms={stats['p90_ms']:>7.1f}"
        )
    instr = report["instructions"]
    lines.append(
        f"instructions resends={instr['resends']} bytes={instr['bytes']:,} "
        f"distinct_content={instr['distinct_content']} repeat_resends={instr['repeat_resends']}"
    )
    lines.append("top_message_templates:")
    for row in report["top_message_templates"]:
        lines.append(
            f"  {row['hook']:<24}{row['hook_event']:<16}count={row['count']:>5} "
            f"bytes={row['output_bytes']:>10,}"
        )
    return "\n".join(lines)


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
