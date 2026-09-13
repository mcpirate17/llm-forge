"""Differential parity twin (Python side) for the Rust port of `_bash_impact.py`.

`native/forge/tests/bash_impact_parity.rs` and this file load the SAME two
fixtures -- `native/forge/tests/fixtures/bash_impact_corpus.json` (`{ROOT}`-
templated command strings) and `bash_impact_expected.json` (frozen
`{command_template, tier, impact_template}` verdicts, captured once from this
very module against the committed fixture tree at
`native/forge/tests/fixtures/bash_impact_tree/`) -- and each substitutes its
own absolute path for `{ROOT}` before asserting its live implementation still
matches the frozen (template-substituted) values. Unlike
`test_guard_parity_corpus.py`'s pure string matching, impact analysis reads
the filesystem, so the corpus points at a real, committed directory tree
instead of an arbitrary repo-root string -- see that tree and
`bash_impact_parity.rs`'s module doc for how to regenerate the fixture if
either module's behaviour changes on purpose.
"""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_MODULE_PATH = _HERE / "_bash_impact.py"
_FIXTURES = (
    _HERE.parent.parent.parent.parent / "native" / "forge" / "tests" / "fixtures"
)
_TREE = _FIXTURES / "bash_impact_tree"


def _load_module():
    spec = importlib.util.spec_from_file_location(
        "bash_impact_parity_under_test", _MODULE_PATH
    )
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _load_json(name: str) -> list[dict]:
    return json.loads((_FIXTURES / name).read_text())


bash_impact = _load_module()


def test_fixture_tree_and_files_exist_and_are_shared_with_the_rust_test() -> None:
    corpus = _load_json("bash_impact_corpus.json")
    expected = _load_json("bash_impact_expected.json")
    assert len(corpus) == len(expected)
    assert len(expected) >= 30, f"expected >= 30 corpus entries, got {len(expected)}"
    assert _TREE.is_dir(), f"fixture tree missing at {_TREE}"


def test_classify_matches_the_frozen_expected_values() -> None:
    root = str(_TREE)
    failures = []
    for entry in _load_json("bash_impact_expected.json"):
        command = entry["command_template"].replace("{ROOT}", root)
        tier, impact_text = bash_impact._classify(command)
        expected_impact = entry["impact_template"].replace("{ROOT}", root)
        if tier != entry["tier"] or impact_text != expected_impact:
            failures.append(
                f"command={entry['command_template']!r}\n"
                f"  expected: tier={entry['tier']} impact={expected_impact!r}\n"
                f"  actual:   tier={tier} impact={impact_text!r}"
            )
    assert not failures, f"{len(failures)} bash_impact mismatches:\n" + "\n".join(
        failures
    )
