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


def main() -> int:
    try:
        payload = json.load(sys.stdin)
        path = Path(os.environ.get("CONTEXT_TELEMETRY_PATH", DEFAULT_PATH))
        if not append(event(payload), path):
            print("context telemetry log is full; event dropped", file=sys.stderr)
    except (OSError, TypeError, ValueError, json.JSONDecodeError) as exc:
        print(f"context telemetry unavailable: {exc}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
