"""`ledger_calibrate`: measure the byte-proportional attribution's error bound.

Design step 3 (`docs/design/cost_ledger.md` section 3, item 3, and section 6
step 3). The rollup splits a turn's billed input tokens across block types by
char share, which assumes uniform chars-per-token across block types --
false at the margin, so the design demands the split ship with a measured
error bound, never a bare number. This module is the Python half of that
measurement; `forge ledger calibrate sample` (Rust) is the other half,
deciding deterministically which turns to measure and emitting only shapes.
This half re-reads the sampled turns' block text from the transcripts (the
one place allowed to see it -- the reader's hard rule forbids text past the
*reader* boundary, and this module is not the reader), resolves each turn's
API-visible input (the messages since the last compaction marker, the
window the harness actually sends), and either:

- **online**: calls the Anthropic `count_tokens` endpoint once per
  block-type group in isolation plus once for the whole input, and reports
  per-block-type MAPE against the proportional estimate the rollup would
  produce for the same bytes; or
- **offline** (`--offline`): reports chars-per-token (`total window chars /
  billed input`) median and p10/p90 per session and overall -- the
  calibration of the rollup's `cpt4` constant -- with no network, no SDK
  and no API key.

The measured (or honestly `null`) bound lands in
`native/forge/tests/fixtures/ledger/calibration.json`, which `rollup.rs`
embeds at build time: a measured `per_block_type` renames every
`turn_attribution` row's `estimate_method` to
`byte_proportional_calibrated_<date>` and stamps the per-block error beside
the estimate; a `null` one keeps the uncalibrated label. Nothing here
fabricates a bound -- `ANTHROPIC_API_KEY` absent and no `--offline` prints
exactly what is missing and exits 2.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import time
from collections import Counter
from pathlib import Path
from typing import Any, Sequence

from pydantic import BaseModel, Field

from conductor.project_paths import host_root

DEFAULT_OUT = "native/forge/tests/fixtures/ledger/calibration.json"
DEFAULT_CACHE = ".ledger-calibrate-cache.json"
DEFAULT_SLEEP_MS = 200
BLOCK_TYPES = ("text", "tool_result", "tool_use", "image", "other")


class SampleRow(BaseModel):
    """One line of `forge ledger calibrate sample` output."""

    session_id: str
    turn_uuid: str
    turn_index: int
    bytes_by_block_type: dict[str, int]
    billed_input: int


class BlockTypeErrorStat(BaseModel):
    """One block type's measured error vs the byte-proportional estimate."""

    mape: float
    n: int
    mean_est: float
    mean_actual: float


class CharsPerToken(BaseModel):
    """Nearest-rank median / p10 / p90 of chars-per-token over the sample."""

    median: float
    p10: float
    p90: float


class CalibrationFixture(BaseModel):
    """The committed artifact `rollup.rs` embeds (`include_str!`)."""

    generated_utc: str
    model: str | None
    n_turns: int
    per_block_type: dict[str, BlockTypeErrorStat] | None
    whole_input_mape: float | None
    chars_per_token: CharsPerToken


class TurnWindow(BaseModel):
    """A sampled turn's resolved API-visible input window."""

    session_id: str
    turn_uuid: str
    model: str | None
    billed_input: int
    chars_by_block_type: dict[str, int]
    # The actual blocks per type, needed only by the online path; never
    # serialized anywhere (this model is a computation carrier, not output).
    payloads: dict[str, list[dict[str, Any]]] = Field(default_factory=dict, exclude=True)


def _percentile(values: Sequence[float], p: float) -> float:
    """Nearest-rank percentile (rank = ceil(p*n), 1-indexed) over an
    unsorted sequence -- the same definition the Rust reader's `percentile`
    uses, so both halves of the ledger quote one percentile rule."""

    ordered = sorted(values)
    rank = min(max(1, math.ceil(p * len(ordered))), len(ordered))
    return ordered[rank - 1]


def cpt_stats(values: Sequence[float]) -> CharsPerToken:
    if not values:
        raise SystemExit("ledger_calibrate: no sampled turns produced a chars-per-token value")
    return CharsPerToken(
        median=_percentile(values, 0.5),
        p10=_percentile(values, 0.10),
        p90=_percentile(values, 0.90),
    )


# --- window resolution -------------------------------------------------------


def _block_type_and_chars(block: dict[str, Any]) -> tuple[str, int]:
    """Mirror the Rust reader's per-block char rules exactly (reader.rs
    `parse_one_block`) so the window's char counts are in the same unit the
    rollup's split uses: text = chars, tool_result = string or array-of-
    text-subblock chars, tool_use = serialized `input` + tool name,
    image = base64 payload chars (0 for URL sources), unknown = serialized
    block chars. Thinking returns 0 chars -- excluded from the input-side
    split by design section 3."""

    kind = block.get("type")
    if kind == "text":
        return "text", len(str(block.get("text") or ""))
    if kind == "tool_result":
        content = block.get("content")
        if isinstance(content, str):
            return "tool_result", len(content)
        if isinstance(content, list):
            chars = sum(
                len(str(item.get("text") or ""))
                for item in content
                if isinstance(item, dict) and item.get("type") == "text"
            )
            return "tool_result", chars
        return "tool_result", 0
    if kind == "tool_use":
        name = str(block.get("name") or "")
        payload = block.get("input")
        serialized = len(json.dumps(payload, separators=(",", ":"))) if payload is not None else 0
        return "tool_use", serialized + len(name)
    if kind == "image":
        source = block.get("source")
        if isinstance(source, dict) and source.get("type") == "base64":
            return "image", len(str(source.get("data") or ""))
        return "image", 0
    if kind == "thinking":
        return "thinking", 0
    return "other", len(json.dumps(block, separators=(",", ":")))


def index_transcript(path: Path) -> tuple[dict[str, int], list[int]]:
    """(uuid -> byte offset of its line, compaction-marker line offsets).

    One cheap streaming pass keeping only uuids, offsets and marker
    positions, never content -- window payloads are read per target below,
    so peak memory is one window, not one file (the design's largest
    session is 100 MB).
    """

    offsets: dict[str, int] = {}
    markers: list[int] = []
    with path.open("rb") as handle:
        offset = 0
        for raw in handle:
            start = offset
            offset += len(raw)
            try:
                line = json.loads(raw)
            except ValueError:
                continue
            if not isinstance(line, dict):
                continue
            uuid = line.get("uuid")
            if isinstance(uuid, str):
                offsets[uuid] = start
            if line.get("isCompactSummary"):
                markers.append(start)
    return offsets, markers


def resolve_windows(sample: list[SampleRow], transcripts: list[Path]) -> list[TurnWindow]:
    """Each sampled turn's API-visible input: the messages from the last
    compaction marker (its own summary message included -- it is what the
    harness sends after compacting) up to but excluding the turn's own
    assistant line."""

    index: dict[Path, tuple[dict[str, int], list[int]]] = {
        path: index_transcript(path) for path in transcripts
    }
    windows: list[TurnWindow] = []
    for row in sample:
        path = next((p for p in transcripts if row.turn_uuid in index[p][0]), None)
        if path is None:
            raise SystemExit(
                f"ledger_calibrate: sampled turn {row.turn_uuid} is in none of the transcripts"
            )
        offsets, markers = index[path]
        target = offsets[row.turn_uuid]
        starts = [marker for marker in markers if marker <= target]
        windows.append(read_window(path, starts[-1] if starts else 0, target, row))
    return windows


def _message_blocks(message: dict[str, Any]) -> list[dict[str, Any]]:
    """A message's content as blocks: a bare string is one text block (how
    the harness writes plain user prompts), a list is its dict entries,
    anything else contributes nothing to the window."""

    content = message.get("content")
    if isinstance(content, str):
        return [{"type": "text", "text": content}]
    if isinstance(content, list):
        return [block for block in content if isinstance(block, dict)]
    return []


def _model_on(line: Any) -> str | None:
    """The model a transcript line's message ran on, if the line says."""

    if not isinstance(line, dict):
        return None
    message = line.get("message")
    if not isinstance(message, dict):
        return None
    model = message.get("model")
    return model if isinstance(model, str) else None


def read_window(path: Path, start: int, end: int, row: SampleRow) -> TurnWindow:
    """Aggregate one window's blocks by type (chars + payloads), streaming
    the byte range [start, end) and stopping at the turn's own line."""

    chars: dict[str, int] = {kind: 0 for kind in (*BLOCK_TYPES, "thinking")}
    payloads: dict[str, list[dict[str, Any]]] = {kind: [] for kind in BLOCK_TYPES}
    model: str | None = None
    with path.open("rb") as handle:
        handle.seek(start)
        position = start
        for raw in handle:
            if position >= end:
                break
            position += len(raw)
            try:
                line = json.loads(raw)
            except ValueError:
                continue
            if not isinstance(line, dict):
                continue
            if line.get("uuid") == row.turn_uuid:
                # A duplicated uuid inside the window (a resumed session
                # replays lines): the first occurrence ends the window.
                model = _model_on(line) or model
                break
            message = line.get("message")
            if not isinstance(message, dict):
                continue
            for block in _message_blocks(message):
                kind, block_chars = _block_type_and_chars(block)
                chars[kind] = chars.get(kind, 0) + block_chars
                if kind in payloads:
                    payloads[kind].append(block)
        if model is None:
            # The target's own line sits just past `end` (the window excludes
            # it): read it alone for the model the turn ran on -- the value a
            # missing `--model` and the fixture's `model` field default to.
            handle.seek(end)
            try:
                model = _model_on(json.loads(handle.readline()))
            except ValueError:
                model = None
    return TurnWindow(
        session_id=row.session_id,
        turn_uuid=row.turn_uuid,
        model=model,
        billed_input=row.billed_input,
        chars_by_block_type=chars,
        payloads=payloads,
    )


# --- offline path (item 4) ---------------------------------------------------


def offline_stats(windows: Sequence[TurnWindow]) -> dict[str, CharsPerToken]:
    """chars_per_token = total window chars (the five split block types,
    thinking excluded) / billed_input, per session and overall."""

    overall: list[float] = []
    per_session: dict[str, list[float]] = {}
    for window in windows:
        total_chars = sum(window.chars_by_block_type[kind] for kind in BLOCK_TYPES)
        if total_chars <= 0 or window.billed_input <= 0:
            continue
        cpt = total_chars / window.billed_input
        per_session.setdefault(window.session_id, []).append(cpt)
        overall.append(cpt)
    if not per_session:
        raise SystemExit("ledger_calibrate: every sampled window was empty")
    return {
        **{session: cpt_stats(values) for session, values in sorted(per_session.items())},
        "overall": cpt_stats(overall),
    }


# --- online path (item 3) ----------------------------------------------------


def _messages_for_group(kind: str, blocks: Sequence[dict[str, Any]]) -> list[dict[str, Any]]:
    """One minimal messages array holding only this group's blocks. tool_use
    blocks are assistant-role content in the real transcript and stay that
    way here; every other group rides a single user message."""

    role = "assistant" if kind == "tool_use" else "user"
    return [{"role": role, "content": list(blocks)}]


def _whole_messages(window: TurnWindow) -> list[dict[str, Any]]:
    blocks = [block for kind in BLOCK_TYPES for block in window.payloads[kind]]
    return [{"role": "user", "content": blocks}]


def count_tokens_cached(
    client: Any,
    model: str,
    messages: list[dict[str, Any]],
    cache_path: Path,
    sleep_ms: int,
) -> tuple[int, bool]:
    """`client.messages.count_tokens` behind a job-local JSON cache keyed by
    the exact request payload, so a rerun costs zero calls. Returns
    `(input_tokens, cache_hit)`."""

    payload = json.dumps({"model": model, "messages": messages}, sort_keys=True)
    key = hashlib.sha256(payload.encode("utf-8")).hexdigest()
    cache: dict[str, int] = {}
    if cache_path.is_file():
        loaded = json.loads(cache_path.read_text(encoding="utf-8"))
        if isinstance(loaded, dict):
            cache = {k: int(v) for k, v in loaded.items() if isinstance(v, (int, float))}
    if key in cache:
        return cache[key], True
    response = client.messages.count_tokens(model=model, messages=messages)
    tokens = int(response.get("input_tokens", 0))
    cache[key] = tokens
    cache_path.write_text(json.dumps(cache, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if sleep_ms > 0:
        time.sleep(sleep_ms / 1000.0)
    return tokens, False


def online_measure(
    windows: Sequence[TurnWindow],
    model: str,
    cache_path: Path,
    sleep_ms: int,
) -> dict[str, Any]:
    """Per block type: count_tokens of the isolated group vs the rollup's
    byte-proportional estimate for the same window; plus the whole input in
    one call. `ANTHROPIC_API_KEY` must already be present (the CLI owns the
    loud exit-2 message)."""

    api_key = os.environ.get("ANTHROPIC_API_KEY")
    if not api_key:
        raise AssertionError("online_measure called without ANTHROPIC_API_KEY")
    import anthropic  # lazily: --offline works without the SDK installed

    client = anthropic.Anthropic(api_key=api_key)
    calls = 0
    est_by_type: dict[str, list[tuple[float, float]]] = {kind: [] for kind in BLOCK_TYPES}
    whole: list[tuple[float, float]] = []
    for window in windows:
        total_chars = sum(window.chars_by_block_type[kind] for kind in BLOCK_TYPES)
        if total_chars <= 0 or window.billed_input <= 0:
            continue
        for kind in BLOCK_TYPES:
            if not window.payloads[kind]:
                continue
            tokens, _hit = count_tokens_cached(
                client, model, _messages_for_group(kind, window.payloads[kind]),
                cache_path, sleep_ms,
            )
            calls += 1
            estimate = window.billed_input * (window.chars_by_block_type[kind] / total_chars)
            est_by_type[kind].append((estimate, float(tokens)))
        whole_tokens, _hit = count_tokens_cached(
            client, model, _whole_messages(window), cache_path, sleep_ms
        )
        calls += 1
        whole.append((float(window.billed_input), float(whole_tokens)))

    return {
        "per_block_type": {kind: summarize(pairs) for kind, pairs in est_by_type.items()
                           if pairs},
        "whole_input_mape": summarize(whole)["mape"] if whole else None,
        "api_calls": calls,
        "api_tokens": int(
            sum(actual for pairs in est_by_type.values() for _est, actual in pairs)
            + sum(actual for _est, actual in whole)
        ),
    }


def summarize(pairs: Sequence[tuple[float, float]]) -> dict[str, Any]:
    """MAPE / n / mean_est / mean_actual over (estimate, actual) pairs where
    the actual is positive -- an actual of 0 has no percentage error to
    quote and is excluded from the bound rather than divided by."""

    usable = [(est, actual) for est, actual in pairs if actual > 0]
    if not usable:
        return {"mape": 0.0, "n": 0, "mean_est": 0.0, "mean_actual": 0.0}
    mape = sum(abs(est - actual) / actual for est, actual in usable) / len(usable)
    return {
        "mape": mape,
        "n": len(usable),
        "mean_est": sum(est for est, _ in usable) / len(usable),
        "mean_actual": sum(actual for _, actual in usable) / len(usable),
    }


# --- CLI ---------------------------------------------------------------------


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sample", type=Path, help="`forge ledger calibrate sample` JSONL output")
    parser.add_argument("transcripts", type=Path, nargs="+", help="transcript JSONL file(s)")
    parser.add_argument("--model", default=None, help="count_tokens model; default: the model "
                        "recorded on each sampled turn (the fixture records the most common one)")
    parser.add_argument("--sleep-ms", type=int, default=DEFAULT_SLEEP_MS,
                        help="polite pause between API calls (default 200)")
    parser.add_argument("--offline", action="store_true",
                        help="no network: chars-per-token stats only, per_block_type null")
    parser.add_argument("--out", type=Path, default=None,
                        help=f"fixture to write (default {DEFAULT_OUT} under the repo root)")
    parser.add_argument("--cache", type=Path, default=Path(DEFAULT_CACHE),
                        help=f"job-local count_tokens cache (default {DEFAULT_CACHE})")
    args = parser.parse_args(argv)

    sample = [SampleRow.model_validate(json.loads(line)) for line in
              args.sample.read_text(encoding="utf-8").splitlines() if line.strip()]
    if not sample:
        raise SystemExit("ledger_calibrate: the sample file holds no turns")
    windows = resolve_windows(sample, args.transcripts)
    offline = offline_stats(windows)

    per_block_type: dict[str, BlockTypeErrorStat] | None = None
    whole_input_mape: float | None = None
    if args.offline:
        print("offline mode: per_block_type bound not measured (no API calls)")
    elif not os.environ.get("ANTHROPIC_API_KEY"):
        print(
            "ledger_calibrate: ANTHROPIC_API_KEY is not set -- the count_tokens "
            "calibration cannot run. Export ANTHROPIC_API_KEY or pass --offline for "
            "the chars-per-token numbers alone.",
            flush=True,
        )
        return 2
    else:
        model = args.model or Counter(w.model for w in windows if w.model).most_common(1)[0][0]
        measured = online_measure(windows, model, args.cache, args.sleep_ms)
        per_block_type = {
            kind: BlockTypeErrorStat(**row) for kind, row in measured["per_block_type"].items()
        }
        whole_input_mape = measured["whole_input_mape"]
        print(f"count_tokens calls: {measured['api_calls']} "
              f"({measured['api_tokens']} tokens counted)")

    models = Counter(w.model for w in windows if w.model)
    fixture = CalibrationFixture(
        generated_utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        model=args.model or (models.most_common(1)[0][0] if models else None),
        n_turns=len(windows),
        per_block_type=per_block_type,
        whole_input_mape=whole_input_mape,
        chars_per_token=offline["overall"],
    )
    out = args.out or host_root() / DEFAULT_OUT
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(
        json.dumps(fixture.model_dump(mode="json"), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    report = {"chars_per_token": offline["overall"].model_dump(),
              "per_session": {name: stats.model_dump()
                              for name, stats in offline.items() if name != "overall"}}
    print(f"fixture: {out}")
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
