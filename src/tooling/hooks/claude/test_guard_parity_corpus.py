"""Differential parity twin (Python side) for the Rust port of `_bash_guard.py`
and `bash_write_targets.py`.

`native/forge/tests/guard_parity.rs` and this file load the SAME two fixtures --
`native/forge/tests/fixtures/guard_parity_corpus.json` (91 command strings) and
`guard_parity_expected.json` (frozen `{command, blocked, reason, write_targets}`
verdicts, captured once from these very Python modules with `repo_root =
Path("/repo")`) -- and each asserts its own live implementation still matches
the frozen values. That pins both implementations to one shared ground truth
instead of comparing them to each other directly at test time, and turns any
future *intentional* behaviour change in either module into a fixture-update
task rather than a silent divergence: change the behaviour on purpose,
regenerate `guard_parity_expected.json` from both modules, then update both
test suites' expectations in the same commit.

Modules are loaded relative to this file, like `test_bash_guard.py` does, so a
mutation snapshot tests its own copy.
"""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_GUARD_PATH = _HERE / "_bash_guard.py"
_WRITE_TARGETS_PATH = _HERE.parent / "agent" / "bash_write_targets.py"
_FIXTURES = (
    _HERE.parent.parent.parent.parent / "native" / "forge" / "tests" / "fixtures"
)


def _load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


guard = _load("bash_guard_parity_under_test", _GUARD_PATH)
write_targets = _load("bash_write_targets_parity_under_test", _WRITE_TARGETS_PATH)


def _load_expected() -> list[dict]:
    raw = (_FIXTURES / "guard_parity_expected.json").read_text()
    return json.loads(raw)


def test_fixture_files_exist_and_are_shared_with_the_rust_test() -> None:
    corpus = json.loads((_FIXTURES / "guard_parity_corpus.json").read_text())
    expected = _load_expected()
    assert len(corpus) == len(expected)
    assert len(expected) >= 76, (
        f"expected >= 36 existing + 40 new = 76 corpus entries, got {len(expected)}"
    )


def test_bash_guard_matches_the_frozen_expected_values() -> None:
    failures = []
    for entry in _load_expected():
        reason = guard.check(entry["command"])
        blocked = reason is not None
        if blocked != entry["blocked"] or reason != entry["reason"]:
            failures.append(
                f"command={entry['command']!r}\n"
                f"  expected: blocked={entry['blocked']} reason={entry['reason']!r}\n"
                f"  actual:   blocked={blocked} reason={reason!r}"
            )
    assert not failures, f"{len(failures)} guard mismatches:\n" + "\n".join(failures)


def test_write_targets_match_the_frozen_expected_values() -> None:
    repo_root = Path("/repo")
    failures = []
    for entry in _load_expected():
        targets = write_targets.repo_write_targets(entry["command"], repo_root)
        if targets != entry["write_targets"]:
            failures.append(
                f"command={entry['command']!r}\n"
                f"  expected: {entry['write_targets']!r}\n"
                f"  actual:   {targets!r}"
            )
    assert not failures, f"{len(failures)} write-target mismatches:\n" + "\n".join(
        failures
    )
