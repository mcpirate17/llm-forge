"""Small deterministic helpers for bounded agent context envelopes.

This module deliberately does not summarize or infer. It only normalizes
whitespace, removes exact duplicate fragments, and applies a hard character
budget before text is handed to an agent.
"""

from __future__ import annotations

import re
from collections.abc import Iterable

_SPACE_RE = re.compile(r"\s+")


def normalized_key(text: str) -> str:
    """Return a stable exact-deduplication key without changing display text."""

    return _SPACE_RE.sub(" ", text).strip().casefold()


def dedupe_fragments(
    fragments: Iterable[str], *, seen: set[str] | None = None
) -> list[str]:
    """Keep the first occurrence of each non-empty fragment."""

    keys = seen if seen is not None else set()
    unique: list[str] = []
    for fragment in fragments:
        if not isinstance(fragment, str):
            continue
        key = normalized_key(fragment)
        if not key or key in keys:
            continue
        keys.add(key)
        unique.append(fragment)
    return unique


def fit_text(text: str, max_chars: int, *, marker: str = "…") -> str:
    """Return text no longer than *max_chars*, with a bounded marker."""

    if max_chars < 1:
        raise ValueError("max_chars must be positive")
    if len(text) <= max_chars:
        return text
    if len(marker) >= max_chars:
        return marker[:max_chars]
    return text[: max_chars - len(marker)].rstrip() + marker
