"""Strict HTTP(S)-only transport boundary for injectable urllib callers."""

from __future__ import annotations

from typing import Any, Callable
import urllib.parse


def open_http(
    opener: Callable[..., Any],
    target: Any,
    *,
    timeout: float,
    error_type: type[Exception] = ValueError,
) -> Any:
    """Open a validated HTTP(S) target through the caller's injectable opener."""
    raw_url = target.full_url if hasattr(target, "full_url") else str(target)
    try:
        parsed = urllib.parse.urlsplit(raw_url)
        port = parsed.port
    except (TypeError, ValueError) as exc:
        raise error_type(f"invalid HTTP target {raw_url!r}: {exc}") from exc
    if parsed.scheme.lower() not in {"http", "https"} or not parsed.hostname:
        raise error_type(f"HTTP target must use http or https: {raw_url!r}")
    if parsed.username is not None or parsed.password is not None:
        raise error_type(f"HTTP target may not contain user info: {raw_url!r}")
    if port is not None and not 1 <= port <= 65535:
        raise error_type(f"HTTP target port is invalid: {raw_url!r}")
    return opener(target, timeout=timeout)
