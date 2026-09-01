#!/usr/bin/env python3
"""Record provider-neutral context-size telemetry without tool contents."""

from __future__ import annotations

import fcntl
import json
import os
import sys
from collections.abc import Iterator, Mapping
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Final

ROOT: Final[Path] = Path(__file__).resolve().parents[1]
DEFAULT_PATH: Final[Path] = (
    ROOT / "research" / "tmp" / "context_telemetry" / "events.jsonl"
)
MAX_LOG_BYTES: Final[int] = 10 * 1024 * 1024
MAX_PROVIDER_CHARS: Final[int] = 32
MAX_TOOL_CHARS: Final[int] = 100
_NATIVE_USAGE_CONTAINER_KEYS: Final[tuple[str, ...]] = (
    "response",
    "metadata",
)


def _provider() -> str:
    if os.environ.get("GROK_WORKSPACE_ROOT") or os.environ.get("GROK_HOOK_EVENT"):
        return "grok"
    if os.environ.get("QWEN_PROJECT_DIR"):
        return "qwen"
    if os.environ.get("CLAUDE_PROJECT_DIR"):
        return "claude"
    if os.environ.get("CODEX_SESSION_ID") or os.environ.get("CODEX_HOME"):
        return "codex"
    return "unknown"


def _json_bytes(value: Any) -> bytes:
    if value is None:
        return b""
    try:
        return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode(
            "utf-8"
        )
    except (TypeError, ValueError):
        return b""


def _field(payload: dict[str, Any], *names: str) -> Any:
    for name in names:
        if name in payload:
            return payload[name]
    return None


def _usage_mappings(payload: Any) -> Iterator[tuple[Mapping[str, Any], str]]:
    """Yield provider-envelope usage containers, never downstream tool output."""

    if not isinstance(payload, Mapping):
        return
    for key in ("usage", "usageMetadata", "usage_metadata", "token_usage"):
        value = payload.get(key)
        if isinstance(value, Mapping):
            yield value, key
    for container_key in _NATIVE_USAGE_CONTAINER_KEYS:
        container = payload.get(container_key)
        if not isinstance(container, Mapping):
            continue
        for usage_key in ("usage", "usageMetadata", "usage_metadata", "token_usage"):
            value = container.get(usage_key)
            if isinstance(value, Mapping):
                yield value, f"{container_key}.{usage_key}"


def _nonnegative_int(value: Any) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value if value >= 0 else None
    if isinstance(value, str) and value.isdecimal():
        return int(value)
    return None


def _usage_value(
    usage: Mapping[str, Any], *names: str
) -> tuple[int | None, str | None]:
    for name in names:
        value = _nonnegative_int(usage.get(name))
        if value is not None:
            return value, name
    return None, None


def _extract_native_usage(payload: Any) -> dict[str, Any]:
    """Normalize known vendor usage fields without retaining response content."""

    for usage, usage_path in _usage_mappings(payload):
        fields: dict[str, Any] = {}
        field_names: set[str] = set()

        def take(output_name: str, *names: str) -> None:
            value, matched = _usage_value(usage, *names)
            if value is not None and matched is not None:
                fields[output_name] = value
                field_names.add(matched)

        take("input_tokens", "input_tokens", "prompt_tokens", "prompt_eval_count")
        take("output_tokens", "output_tokens", "completion_tokens", "eval_count")
        take(
            "cached_input_tokens",
            "cached_tokens",
            "cache_read_input_tokens",
            "cache_read_tokens",
        )
        take(
            "cache_creation_input_tokens",
            "cache_creation_input_tokens",
            "cache_creation_tokens",
        )
        take("reasoning_tokens", "reasoning_tokens")
        take("total_tokens", "total_tokens")

        for detail_key in (
            "prompt_tokens_details",
            "input_tokens_details",
            "promptTokenDetails",
        ):
            details = usage.get(detail_key)
            if isinstance(details, Mapping):
                value, matched = _usage_value(
                    details,
                    "cached_tokens",
                    "cache_read_input_tokens",
                    "cache_read_tokens",
                )
                if value is not None and "cached_input_tokens" not in fields:
                    fields["cached_input_tokens"] = value
                    field_names.add(f"{detail_key}.{matched}")

        for detail_key in (
            "completion_tokens_details",
            "output_tokens_details",
            "completionTokenDetails",
        ):
            details = usage.get(detail_key)
            if isinstance(details, Mapping):
                value, matched = _usage_value(details, "reasoning_tokens")
                if value is not None and "reasoning_tokens" not in fields:
                    fields["reasoning_tokens"] = value
                    field_names.add(f"{detail_key}.{matched}")

        if fields:
            return {
                **fields,
                "usage_source": "native",
                "native_usage_path": usage_path,
                "native_usage_fields": sorted(field_names),
            }
    return {
        "input_tokens": None,
        "output_tokens": None,
        "cached_input_tokens": None,
        "cache_creation_input_tokens": None,
        "reasoning_tokens": None,
        "total_tokens": None,
        "usage_source": "none",
        "native_usage_path": None,
        "native_usage_fields": [],
    }


MODEL_VISIBLE_FIELDS: Final[dict[str, tuple[str, ...]]] = {
    "Edit": ("filePath", "structuredPatch", "userModified"),
    "Write": ("type", "filePath"),
}


def model_visible_output(tool_name: str, tool_output: Any) -> Any:
    """Project a tool response onto what the agent actually sees.

    Edit and Write echo the whole file back to the hook (``originalFile``,
    ``content``) while the agent sees a confirmation and the patch; measuring
    the raw envelope credited Edit with 22 % of all tool bytes (2026-09-01).
    Other tools are measured as delivered.
    """

    fields = MODEL_VISIBLE_FIELDS.get(tool_name)
    if fields is None or not isinstance(tool_output, Mapping):
        return tool_output
    return {key: tool_output[key] for key in fields if key in tool_output}


def _is_bounded_output(value: Any, encoded: bytes) -> bool:
    if isinstance(value, Mapping):
        for key in ("elided", "truncated", "output_bounded", "outputBounded"):
            marker = value.get(key)
            if marker is True or (isinstance(marker, str) and marker):
                return True
    lowered = encoded.lower()
    return b'"elided"' in lowered or b'"truncated"' in lowered


def event(payload: Any) -> dict[str, Any]:
    """Reduce one hook payload to counts and non-sensitive routing labels."""

    if not isinstance(payload, dict):
        payload = {}
    tool_input = _field(payload, "tool_input", "toolInput")
    tool_output = _field(
        payload,
        "tool_response",
        "toolResult",
        "tool_output",
        "toolOutput",
        "tool_result",
    )
    if tool_output is None and isinstance(payload, dict):
        tool_output = payload.get("output")
    event_name = str(
        _field(payload, "hook_event_name", "hookEventName") or "PostToolUse"
    )
    tool_name = str(_field(payload, "tool_name", "toolName") or "unknown")
    tool_output = model_visible_output(tool_name, tool_output)
    input_encoded = _json_bytes(tool_input)
    output_encoded = _json_bytes(tool_output)
    input_bytes = len(input_encoded)
    output_bytes = len(output_encoded)
    native_usage = _extract_native_usage(payload)
    return {
        "timestamp": datetime.now(timezone.utc).isoformat(timespec="milliseconds"),
        "pid": os.getpid(),
        "provider": _provider()[:MAX_PROVIDER_CHARS],
        "event": event_name[:MAX_TOOL_CHARS],
        "tool": tool_name[:MAX_TOOL_CHARS],
        "input_bytes": input_bytes,
        "output_bytes": output_bytes,
        "tool_input_tokens_estimate": (input_bytes + 3) // 4,
        "output_tokens_estimate": (output_bytes + 3) // 4,
        "output_tokens_estimate_scope": "tool-output-bytes",
        "output_bounded": _is_bounded_output(tool_output, output_encoded),
        **native_usage,
    }


def append(record: dict[str, Any], path: Path = DEFAULT_PATH) -> bool:
    """Append one record while stopping cleanly at the bounded log budget."""

    encoded = (
        json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n"
    ).encode("utf-8")
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a+b") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            handle.seek(0, os.SEEK_END)
            if handle.tell() + len(encoded) > MAX_LOG_BYTES:
                return False
            handle.write(encoded)
            handle.flush()
            return True
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def rotate(path: Path) -> Path:
    """Move a full log aside as ``<name>.<utc-stamp>.full`` and return the new name."""

    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    target = path.with_name(f"{path.name}.{stamp}.full")
    os.replace(path, target)
    return target


def record(record_: dict[str, Any], path: Path) -> None:
    """Append, rotating a full log first: the cap bounds one file, never drops data."""

    if append(record_, path):
        return
    rotated = rotate(path)
    print(f"context telemetry log rotated to {rotated.name}", file=sys.stderr)
    if not append(record_, path):
        raise OSError(f"context telemetry record exceeds the log budget ({path})")


def hook_context_event(
    hook: str, hook_json: Any, *, event_name: str = ""
) -> dict[str, Any]:
    """Reduce one hook's stdout JSON to the size of the context it injects."""

    context = ""
    specific = (
        hook_json.get("hookSpecificOutput") if isinstance(hook_json, dict) else None
    )
    if isinstance(specific, dict):
        # A deny reason reaches the agent exactly like injected context does.
        context = str(
            specific.get("additionalContext")
            or specific.get("permissionDecisionReason")
            or ""
        )
        event_name = event_name or str(specific.get("hookEventName") or "")
    context_bytes = len(context.encode("utf-8"))
    return {
        "timestamp": datetime.now(timezone.utc).isoformat(timespec="milliseconds"),
        "pid": os.getpid(),
        "provider": _provider()[:MAX_PROVIDER_CHARS],
        "event": "HookContext",
        "hook_event": (event_name or "unknown")[:MAX_TOOL_CHARS],
        "tool": hook[:MAX_TOOL_CHARS],
        "input_bytes": 0,
        "output_bytes": context_bytes,
        "tool_input_tokens_estimate": 0,
        "output_tokens_estimate": (context_bytes + 3) // 4,
        "output_tokens_estimate_scope": "hook-additional-context-bytes",
        "output_bounded": False,
    }


def _events(paths: list[Path]) -> Iterator[dict[str, Any]]:
    for path in paths:
        with path.open("rb") as handle:
            for line in handle:
                try:
                    item = json.loads(line)
                except ValueError:
                    continue
                if isinstance(item, dict):
                    yield item


def summarize(paths: list[Path], *, bound_bytes: int = 8000) -> dict[str, Any]:
    """Per-(event, tool) byte and token totals; ``over_bound`` = raw results the
    post-bash-quiet hook cuts down before the model sees them."""

    rows: dict[tuple[str, str], dict[str, int]] = {}
    for item in _events(paths):
        key = (str(item.get("event", "?")), str(item.get("tool", "?")))
        row = rows.setdefault(
            key,
            {
                "count": 0,
                "output_bytes": 0,
                "output_tokens_estimate": 0,
                "over_bound": 0,
                "over_bound_bytes": 0,
            },
        )
        out = _nonnegative_int(item.get("output_bytes")) or 0
        row["count"] += 1
        row["output_bytes"] += out
        row["output_tokens_estimate"] += (
            _nonnegative_int(item.get("output_tokens_estimate")) or 0
        )
        if out > bound_bytes:
            row["over_bound"] += 1
            row["over_bound_bytes"] += out - bound_bytes
    ranked = sorted(rows.items(), key=lambda kv: kv[1]["output_bytes"], reverse=True)
    total = sum(r["output_bytes"] for r in rows.values())
    return {
        "schema_version": "llm.context-telemetry.summary.v1",
        "files": [str(p) for p in paths],
        "bound_bytes": bound_bytes,
        "events": sum(r["count"] for r in rows.values()),
        "output_bytes": total,
        "hook_context_bytes": sum(
            r["output_bytes"] for (ev, _), r in rows.items() if ev == "HookContext"
        ),
        "rows": [
            {
                "event": ev,
                "tool": tool,
                **r,
                "share": round(r["output_bytes"] / total, 4) if total else 0.0,
            }
            for (ev, tool), r in ranked
        ],
    }


def _format_summary(summary: dict[str, Any]) -> str:
    lines = [
        f"events={summary['events']} output_bytes={summary['output_bytes']:,} "
        f"hook_context_bytes={summary['hook_context_bytes']:,} bound={summary['bound_bytes']}",
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
