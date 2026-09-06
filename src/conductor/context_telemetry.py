"""Record provider-neutral context-size telemetry without tool contents."""

from __future__ import annotations

import json
import os
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Final

ROOT: Final[Path] = Path(__file__).resolve().parents[1]
DEFAULT_PATH: Final[Path] = (
    ROOT / "research" / "tmp" / "context_telemetry" / "events.jsonl"
)
MAX_LOG_BYTES: Final[int] = 10 * 1024 * 1024


def event(payload: Any) -> dict[str, Any]:
    """Reduce one hook payload to counts and non-sensitive routing labels."""

    from conductor._native import context_telemetry_event_native

    return context_telemetry_event_native(
        payload,
        datetime.now(UTC).isoformat(timespec="milliseconds"),
    )


def _encoded_record(record: dict[str, Any]) -> bytes:
    return (
        json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n"
    ).encode("utf-8")


def append(record: dict[str, Any], path: Path = DEFAULT_PATH) -> bool:
    """Append one record while stopping cleanly at the bounded log budget."""

    from conductor._native import context_telemetry_append_native

    return context_telemetry_append_native(
        str(path), _encoded_record(record), MAX_LOG_BYTES
    )


def rotate(path: Path) -> Path:
    """Move a full log aside as ``<name>.<utc-stamp>.full`` and return the new name."""

    from conductor._native import context_telemetry_rotate_native

    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    return Path(context_telemetry_rotate_native(str(path), stamp))


def record(record_: dict[str, Any], path: Path) -> None:
    """Append, rotating a full log first: the cap bounds one file, never drops data."""

    from conductor._native import context_telemetry_record_native

    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    rotated = context_telemetry_record_native(
        str(path), _encoded_record(record_), MAX_LOG_BYTES, stamp
    )
    if rotated is not None:
        print(f"context telemetry log rotated to {Path(rotated).name}", file=sys.stderr)


def hook_context_event(
    hook: str, hook_json: Any, *, event_name: str = ""
) -> dict[str, Any]:
    """Reduce one hook's stdout JSON to the size of the context it injects."""

    from conductor._native import context_telemetry_hook_event_native

    return context_telemetry_hook_event_native(
        hook,
        hook_json,
        event_name,
        datetime.now(UTC).isoformat(timespec="milliseconds"),
    )


def summarize(paths: list[Path], *, bound_bytes: int = 8000) -> dict[str, Any]:
    """Per-(event, tool) byte and token totals; ``over_bound`` = raw results the
    post-bash-quiet hook cuts down before the model sees them."""

    from conductor._native import context_telemetry_summarize_native

    return json.loads(
        context_telemetry_summarize_native([str(path) for path in paths], bound_bytes)
    )


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


def _cmd_record(path: Path) -> int:
    try:
        record(event(json.load(sys.stdin)), path)
    except (OSError, TypeError, ValueError) as exc:
        print(f"context telemetry unavailable: {exc}", file=sys.stderr)
    return 0


def _cmd_hook_context(path: Path, hook: str, event_name: str) -> int:
    """Tee: log the injected-context size, echo the hook JSON byte-for-byte."""

    raw = sys.stdin.buffer.read()
    sys.stdout.buffer.write(raw)
    sys.stdout.flush()
    try:
        item = hook_context_event(hook, json.loads(raw), event_name=event_name)
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
    summary = sub.add_parser("summary", help="aggregate one or more event logs")
    summary.add_argument("paths", nargs="*", type=Path)
    summary.add_argument("--bound-bytes", type=int, default=8000)
    summary.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    path = Path(os.environ.get("CONTEXT_TELEMETRY_PATH", DEFAULT_PATH))
    if args.command == "hook-context":
        return _cmd_hook_context(path, args.hook, args.event)
    if args.command == "summary":
        paths = args.paths or [path]
        result = summarize(paths, bound_bytes=args.bound_bytes)
        print(json.dumps(result, indent=2) if args.json else _format_summary(result))
        return 0
    return _cmd_record(path)


if __name__ == "__main__":
    raise SystemExit(main())
