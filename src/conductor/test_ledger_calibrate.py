"""Paired tests for conductor.ledger_calibrate (design step 3).

The online path is tested against recorded ``count_tokens`` responses through
a stub ``anthropic`` module -- no network, no SDK, and no API key. The exit-2
path, the offline chars-per-token math, window resolution across a compaction
marker, and the Rust reader's per-block char rules (mirrored here so the two
halves of the calibration stay in one unit) are exercised directly.
"""

from __future__ import annotations

import json
import sys
import types
from pathlib import Path

import pytest

from conductor import ledger_calibrate as lc


# --- char rules (mirror of the Rust reader) ----------------------------------


def test_block_type_and_chars_mirrors_the_rust_reader_rules():
    tool_use_expected = len(json.dumps({"a": 1}, separators=(",", ":"))) + len("Bash")
    other_expected = len(json.dumps({"type": "document", "x": 1}, separators=(",", ":")))
    cases = [
        # text: literal chars
        ({"type": "text", "text": "héllo"}, ("text", 5)),
        # tool_result: string content is its chars
        ({"type": "tool_result", "content": "abcd"}, ("tool_result", 4)),
        # tool_result: array content sums text subblocks only
        (
            {"type": "tool_result", "content": [
                {"type": "text", "text": "ab"},
                {"type": "image", "source": {"type": "url"}},
                {"type": "text", "text": "c"},
            ]},
            ("tool_result", 3),
        ),
        # tool_result: any other shape is 0, never a crash
        ({"type": "tool_result", "content": 7}, ("tool_result", 0)),
        # tool_use: serialized input + tool name
        ({"type": "tool_use", "name": "Bash", "input": {"a": 1}}, ("tool_use", tool_use_expected)),
        ({"type": "tool_use", "name": "Read"}, ("tool_use", 4)),
        # image: base64 payload chars, URL sources carry none
        ({"type": "image", "source": {"type": "base64", "data": "QUJD"}}, ("image", 4)),
        ({"type": "image", "source": {"type": "url", "url": "https://x/y.png"}}, ("image", 0)),
        # thinking is excluded from the input-side split
        ({"type": "thinking", "thinking": "long"}, ("thinking", 0)),
        # unknown block: serialized length
        ({"type": "document", "x": 1}, ("other", other_expected)),
    ]
    for block, expected in cases:
        assert lc._block_type_and_chars(block) == expected, block


# --- window resolution --------------------------------------------------------


def _assistant_line(uuid: str, model: str = "claude-sonnet-5") -> str:
    return json.dumps({
        "uuid": uuid,
        "type": "assistant",
        "message": {
            "role": "assistant",
            "model": model,
            "usage": {"input_tokens": 1, "output_tokens": 1},
            "content": [{"type": "text", "text": "x"}],
        },
    })


def _sample_row(uuid: str, billed: int, session: str = "s") -> str:
    return json.dumps({
        "session_id": session,
        "turn_uuid": uuid,
        "turn_index": 0,
        "bytes_by_block_type": {"text": 1, "tool_result": 0, "tool_use": 0,
                                "image": 0, "other": 0, "thinking": 0},
        "billed_input": billed,
    })


def _tiny_transcript(tmp_path: Path) -> Path:
    path = tmp_path / "t.jsonl"
    path.write_text(
        json.dumps({"type": "user", "message": {
            "role": "user", "content": [{"type": "text", "text": "hello"}]}}) + "\n"
        + _assistant_line("t1") + "\n",
        encoding="utf-8",
    )
    return path


def test_window_starts_at_the_last_compaction_marker(tmp_path: Path):
    path = tmp_path / "t.jsonl"
    path.write_text(
        # Pre-marker content: outside the window.
        json.dumps({"type": "user", "message": {
            "role": "user", "content": [{"type": "text", "text": "old world"}]}}) + "\n"
        + _assistant_line("a1") + "\n"
        # The marker's own summary message is what the harness sends after
        # compacting, so it is inside the window (inclusive).
        + json.dumps({"uuid": "m1", "isCompactSummary": True, "message": {
            "role": "user", "content": [{"type": "text", "text": "SUM"}]}}) + "\n"
        + json.dumps({"type": "user", "message": {"role": "user", "content": [
            {"type": "text", "text": "hello"},
            {"type": "tool_result", "content": "ok"},
        ]}}) + "\n"
        # The target: the window ends at its own line (exclusive).
        + _assistant_line("a2") + "\n"
        # Later turns: outside the window.
        + _assistant_line("a3") + "\n",
        encoding="utf-8",
    )
    row = lc.SampleRow.model_validate(json.loads(_sample_row("a2", 20)))
    (window,) = lc.resolve_windows([row], [path])
    assert window.chars_by_block_type == {
        "text": 8, "tool_result": 2, "tool_use": 0, "image": 0, "other": 0, "thinking": 0,
    }
    assert window.model == "claude-sonnet-5"
    assert window.payloads["text"] == [
        {"type": "text", "text": "SUM"}, {"type": "text", "text": "hello"},
    ]


def test_a_turn_missing_from_every_transcript_fails_loud(tmp_path: Path):
    path = _tiny_transcript(tmp_path)
    row = lc.SampleRow.model_validate(json.loads(_sample_row("nope", 10)))
    with pytest.raises(SystemExit, match="none of the transcripts"):
        lc.resolve_windows([row], [path])


# --- offline path -------------------------------------------------------------


def test_nearest_rank_percentiles_match_the_rust_reader():
    values = [5.0, 1.0, 4.0, 2.0, 3.0]
    assert lc._percentile(values, 0.5) == 3.0
    assert lc._percentile(values, 0.10) == 1.0
    assert lc._percentile(values, 0.90) == 5.0
    # Even n takes the lower middle (rank = ceil(p*n), 1-indexed).
    assert lc._percentile([1.0, 2.0, 3.0, 4.0], 0.5) == 2.0
    with pytest.raises(SystemExit):
        lc.cpt_stats([])


def _window(session: str, billed: int, text_chars: int) -> lc.TurnWindow:
    return lc.TurnWindow(
        session_id=session, turn_uuid=f"{session}-{billed}", model="m", billed_input=billed,
        chars_by_block_type={"text": text_chars, "tool_result": 0, "tool_use": 0,
                             "image": 0, "other": 0, "thinking": 0},
        payloads={kind: [] for kind in lc.BLOCK_TYPES},
    )


def test_offline_stats_group_per_session_and_overall():
    stats = lc.offline_stats([
        _window("a", 10, 5), _window("a", 10, 7), _window("b", 4, 2),
    ])
    assert set(stats) == {"a", "b", "overall"}
    # session a: cpts 0.5 and 0.7 -> nearest-rank median is rank ceil(0.5*2)=1
    # -> the lower value 0.5; p90 is rank ceil(1.8)=2 -> 0.7.
    assert stats["a"].median == 0.5
    assert stats["a"].p10 == 0.5
    assert stats["a"].p90 == 0.7
    assert stats["b"].median == 0.5
    # overall: 0.5, 0.7, 0.5 -> median 0.5, p90 0.7
    assert stats["overall"].median == 0.5
    assert stats["overall"].p90 == 0.7
    # Empty or unbilled windows are skipped, not fabricate a cpt.
    with pytest.raises(SystemExit):
        lc.offline_stats([_window("a", 10, 0)])


# --- CLI: the debt path and the fixture shape ---------------------------------


def test_main_without_a_key_and_without_offline_exits_2(tmp_path, monkeypatch, capsys):
    sample = tmp_path / "sample.jsonl"
    sample.write_text(_sample_row("t1", 10) + "\n", encoding="utf-8")
    transcript = _tiny_transcript(tmp_path)
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    rc = lc.main([str(sample), str(transcript), "--out", str(tmp_path / "f.json")])
    assert rc == 2
    assert "ANTHROPIC_API_KEY is not set" in capsys.readouterr().out
    assert not (tmp_path / "f.json").exists()


def test_main_offline_writes_the_null_bound_fixture(tmp_path, monkeypatch, capsys):
    sample = tmp_path / "sample.jsonl"
    sample.write_text(_sample_row("t1", 10) + "\n", encoding="utf-8")
    transcript = _tiny_transcript(tmp_path)
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    out = tmp_path / "f.json"
    rc = lc.main([str(sample), str(transcript), "--offline", "--out", str(out)])
    assert rc == 0
    assert "offline mode" in capsys.readouterr().out
    fixture = json.loads(out.read_text(encoding="utf-8"))
    # The honest debt shape: no bound measured, everything bound-shaped null.
    assert fixture["per_block_type"] is None
    assert fixture["whole_input_mape"] is None
    assert fixture["model"] == "claude-sonnet-5"
    assert fixture["n_turns"] == 1
    # window chars 5, billed 10 -> cpt 0.5
    assert fixture["chars_per_token"]["median"] == 0.5
    # generated_utc must carry the YYYY-MM-DD prefix the Rust parser demands.
    assert len(fixture["generated_utc"]) >= 10 and fixture["generated_utc"][4] == "-"


# --- online path against recorded responses -----------------------------------


class _RecordedAnthropic:
    """Stands in for ``anthropic.Anthropic``: an ``.messages.count_tokens``
    that plays back recorded responses in call order and records requests."""

    def __init__(self, responses: list[int]):
        self._responses = list(responses)
        self.calls: list[tuple[str, list[dict]]] = []

    @property
    def messages(self) -> _RecordedAnthropic:
        return self

    def count_tokens(self, model: str, messages: list[dict]) -> dict:
        self.calls.append((model, messages))
        return {"input_tokens": self._responses.pop(0)}


def _full_window() -> lc.TurnWindow:
    return lc.TurnWindow(
        session_id="s", turn_uuid="u", model="m", billed_input=100,
        chars_by_block_type={"text": 60, "tool_result": 40, "tool_use": 0,
                             "image": 0, "other": 0, "thinking": 0},
        payloads={
            "text": [{"type": "text", "text": "x" * 60}],
            "tool_result": [{"type": "tool_result", "content": "y" * 40}],
            "tool_use": [], "image": [], "other": [],
        },
    )


def _install_anthropic_stub(monkeypatch, client: _RecordedAnthropic) -> None:
    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key")
    monkeypatch.setitem(sys.modules, "anthropic",
                        types.SimpleNamespace(Anthropic=lambda api_key: client))


def test_online_measure_reports_mape_against_recorded_responses(tmp_path, monkeypatch):
    # text group answers 50 (est 60), tool_result 80 (est 40), whole 90.
    client = _RecordedAnthropic([50, 80, 90])
    _install_anthropic_stub(monkeypatch, client)
    measured = lc.online_measure([_full_window()], "m", tmp_path / "cache.json", sleep_ms=0)
    assert measured["api_calls"] == 3
    assert measured["api_tokens"] == 50 + 80 + 90
    assert measured["per_block_type"]["text"] == {
        "mape": pytest.approx((60 - 50) / 50), "n": 1,
        "mean_est": 60.0, "mean_actual": 50.0,
    }
    assert measured["per_block_type"]["tool_result"]["mape"] == pytest.approx(0.5)
    assert "tool_use" not in measured["per_block_type"]
    assert measured["whole_input_mape"] == pytest.approx(10 / 90)
    # This window has no tool_use blocks, so every request is user-role.
    roles = [messages[0]["role"] for _model, messages in client.calls]
    assert roles == ["user", "user", "user"]  # text, tool_result, whole


def test_group_isolation_roles_match_the_transcript():
    tool_use = [{"type": "tool_use", "name": "Bash", "input": {}}]
    assert lc._messages_for_group("tool_use", tool_use) == [
        {"role": "assistant", "content": tool_use},
    ]
    text = [{"type": "text", "text": "x"}]
    assert lc._messages_for_group("text", text) == [{"role": "user", "content": text}]


def test_count_tokens_cache_makes_a_rerun_cost_zero_calls(tmp_path, monkeypatch):
    first = _RecordedAnthropic([50])
    _install_anthropic_stub(monkeypatch, first)
    cache = tmp_path / "cache.json"
    messages = [{"role": "user", "content": [{"type": "text", "text": "x"}]}]
    tokens, hit = lc.count_tokens_cached(first, "m", messages, cache, sleep_ms=0)
    assert (tokens, hit) == (50, False)
    assert len(first.calls) == 1

    second = _RecordedAnthropic([999])  # would answer wrong if actually called
    tokens, hit = lc.count_tokens_cached(second, "m", messages, cache, sleep_ms=0)
    assert (tokens, hit) == (50, True)
    assert second.calls == []

    # A different payload is a different key and does call.
    other = [{"role": "user", "content": [{"type": "text", "text": "y"}]}]
    third = _RecordedAnthropic([7])
    tokens, hit = lc.count_tokens_cached(third, "m", other, cache, sleep_ms=0)
    assert (tokens, hit) == (7, False)


def test_summarize_excludes_zero_actuals():
    pairs = [(10.0, 0.0), (60.0, 50.0)]
    assert lc.summarize(pairs) == {
        "mape": pytest.approx(0.2), "n": 1, "mean_est": 60.0, "mean_actual": 50.0,
    }
    assert lc.summarize([]) == {"mape": 0.0, "n": 0, "mean_est": 0.0, "mean_actual": 0.0}
