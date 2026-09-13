"""Differential parity twin (Python side) for the Rust port of the two
`PostToolUse` output-bounding hooks: `_bash_quiet` (Bash) and
`post_tool_quiet` (Read/Grep/MCP), native in `native/forge/src/tool_quiet.rs`.

`native/forge/tests/tool_quiet_parity.rs` and this file load the SAME two
fixtures -- `tool_quiet_corpus.json` (38 case descriptors: a `kind`
discriminator, the raw hook payload, and optional `limit_bytes`/`cap_bytes`/
`output_field` overrides) and `tool_quiet_expected.json` (frozen envelopes,
captured once from these very Python modules under a deterministic,
injected environment) -- and each independently recomputes the envelope
for every case, asserting its own live implementation still matches the
frozen values. That pins both implementations to one shared ground truth
instead of comparing them to each other at test time, following the same
shape as `test_bash_pretooluse_hooks_parity_corpus.py`.

Determinism matches the fixture generator exactly: a fixed `_now_stamp()`
(`"20260101T000000"`) and `REPO_ROOT`/`SAVE_DIR` set to a fresh scratch
directory with `_tq_scratch` beneath it as the save dir per case -- since
the spill path is always displayed relative to `REPO_ROOT`, the resulting
marker string is identical regardless of the scratch directory's real
absolute path on whatever machine runs the test.
"""

from __future__ import annotations

import contextlib
import json
import os
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _bash_quiet as bq  # noqa: E402
import post_tool_quiet as ptq  # noqa: E402

_HERE = Path(__file__).resolve().parent
_FIXTURES = (
    _HERE.parent.parent.parent.parent / "native" / "forge" / "tests" / "fixtures"
)
_NOW_STAMP = "20260101T000000"


def _load_json(name: str):
    return json.loads((_FIXTURES / name).read_text())


def test_fixture_files_exist_and_are_shared_with_the_rust_test() -> None:
    corpus = _load_json("tool_quiet_corpus.json")
    expected = _load_json("tool_quiet_expected.json")
    assert len(corpus) == len(expected)
    assert len(corpus) >= 26, (
        f"expected at least 6 existing + 20 new cases (26+ total), got {len(corpus)}"
    )


@contextlib.contextmanager
def _injected_env(repo_root: Path, *, limit_bytes, output_field):
    old_root, old_save, old_limit, old_field, old_stamp = (
        bq.REPO_ROOT,
        bq.SAVE_DIR,
        bq.LIMIT_BYTES,
        bq.OUTPUT_FIELD,
        bq._now_stamp,
    )
    bq.REPO_ROOT = repo_root
    bq.SAVE_DIR = repo_root / "_tq_scratch"
    if limit_bytes is not None:
        bq.LIMIT_BYTES = limit_bytes
    if output_field is not None:
        bq.OUTPUT_FIELD = output_field
    bq._now_stamp = lambda: _NOW_STAMP
    try:
        yield
    finally:
        bq.REPO_ROOT, bq.SAVE_DIR, bq.LIMIT_BYTES, bq.OUTPUT_FIELD, bq._now_stamp = (
            old_root,
            old_save,
            old_limit,
            old_field,
            old_stamp,
        )


@contextlib.contextmanager
def _injected_cap(cap_bytes):
    old = os.environ.get("TOOL_OUTPUT_QUIET_BYTES")
    if cap_bytes is not None:
        os.environ["TOOL_OUTPUT_QUIET_BYTES"] = str(cap_bytes)
    elif "TOOL_OUTPUT_QUIET_BYTES" in os.environ:
        del os.environ["TOOL_OUTPUT_QUIET_BYTES"]
    try:
        yield
    finally:
        if old is None:
            os.environ.pop("TOOL_OUTPUT_QUIET_BYTES", None)
        else:
            os.environ["TOOL_OUTPUT_QUIET_BYTES"] = old


def _run_case(tmp_path: Path, case: dict):
    repo_root = Path(tempfile.mkdtemp(dir=tmp_path, prefix=f"{case['id']}-"))
    limit_bytes = case.get("limit_bytes")
    cap_bytes = case.get("cap_bytes")
    output_field = case.get("output_field")
    with _injected_cap(cap_bytes):
        with _injected_env(
            repo_root, limit_bytes=limit_bytes, output_field=output_field
        ):
            if case["kind"] == "bash_quiet":
                return bq.hook_output(case["payload"])
            if case["kind"] == "post_tool_quiet":
                return ptq.hook_output(case["payload"])
            raise ValueError(f"unknown kind in corpus case: {case['kind']!r}")


def test_python_hooks_match_the_frozen_corpus(tmp_path: Path) -> None:
    corpus = _load_json("tool_quiet_corpus.json")
    expected = _load_json("tool_quiet_expected.json")
    failures = []
    for case in corpus:
        actual = _run_case(tmp_path, case)
        expected_envelope = expected[case["id"]]
        if actual != expected_envelope:
            failures.append(
                f"case {case['id']!r}: python={actual!r} expected={expected_envelope!r}"
            )
    assert not failures, f"{len(failures)} parity mismatches:\n" + "\n".join(failures)
