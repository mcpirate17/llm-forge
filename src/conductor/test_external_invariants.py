"""Tests for the external-invariant waiver admission path.

``external_invariants.evaluate`` decides whether a test that pins an upstream
(e.g. ATen) property may be admitted by the value gate even though it kills no
mutant here. It admits ONLY when it verifies, never on trust: the nodeid must
execute no repository source beyond importing its module, the declared torch
pin must equal the installed version exactly, and the justification must be a
non-empty string. Everything else -- malformed declarations, a stale pin, an
unmeasurable coverage run -- must refuse.

``_measure`` and ``_installed_torch_version`` are monkeypatched throughout so
these tests are fast and hermetic: no real pytest+coverage subprocess runs.
"""

from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Mapping, Sequence

import pytest

from conductor.candidate_review import external_invariants
from conductor.candidate_review.external_invariants import WaiverOutcome, evaluate

TORCH_VERSION = "2.13.0+cu130"
NODEID = "research/tests/test_x.py::test_aten_sum_is_order_stable"
GATED = (NODEID,)


def _declaration(
    *,
    nodeid: object = NODEID,
    justification: object = "pins ATen sum(dim=2) accumulation order",
    pinned: object = {"torch": TORCH_VERSION},
) -> dict[str, object]:
    return {"nodeid": nodeid, "justification": justification, "pinned": pinned}


def _stub_measure(
    calls: Mapping[str, tuple[frozenset[tuple[str, int]], str | None]],
) -> object:
    """A ``_measure`` replacement keyed by invocation label ("import"/"call")."""

    def _fake(
        snapshot: Path, runtime_dir: Path, label: str, pytest_args: list[str]
    ) -> tuple[frozenset[tuple[str, int]], str | None]:
        assert label in calls, f"unexpected _measure label {label!r}"
        return calls[label]

    return _fake


def _never_measure() -> object:
    def _fake(
        snapshot: Path, runtime_dir: Path, label: str, pytest_args: list[str]
    ) -> tuple[frozenset[tuple[str, int]], str | None]:
        raise AssertionError(f"_measure must not run for label {label!r}")

    return _fake


def _raising_measure(exc: BaseException) -> object:
    def _fake(
        snapshot: Path, runtime_dir: Path, label: str, pytest_args: list[str]
    ) -> tuple[frozenset[tuple[str, int]], str | None]:
        raise exc

    return _fake


def _evaluate(
    monkeypatch: pytest.MonkeyPatch,
    *,
    declarations: Sequence[Mapping[str, object]],
    measure: object,
    installed_torch: str | None = TORCH_VERSION,
    gated_nodeids: Sequence[str] = GATED,
    tmp_path: Path,
) -> list[WaiverOutcome]:
    monkeypatch.setattr(external_invariants, "_measure", measure)
    monkeypatch.setattr(
        external_invariants, "_installed_torch_version", lambda: installed_torch
    )
    return evaluate(
        tmp_path / "snapshot", tmp_path / "runtime", declarations, gated_nodeids
    )


def test_evaluate_admits_when_invariant_is_verified_clean(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    baseline = frozenset({("research/synthesis/x.py", 10)})
    measure = _stub_measure(
        {
            # The call re-executes the same import-time lines and nothing more.
            "import": (baseline, None),
            "call": (baseline, None),
        }
    )
    [outcome] = _evaluate(
        monkeypatch, declarations=[_declaration()], measure=measure, tmp_path=tmp_path
    )
    assert outcome.nodeid == NODEID
    assert outcome.admitted is True
    assert "torch pinned at" in outcome.reason


def test_evaluate_refuses_when_call_touches_repo_source_beyond_import(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    baseline = frozenset({("research/synthesis/x.py", 10)})
    call = baseline | frozenset({("research/synthesis/x.py", 42)})
    measure = _stub_measure({"import": (baseline, None), "call": (call, None)})
    [outcome] = _evaluate(
        monkeypatch, declarations=[_declaration()], measure=measure, tmp_path=tmp_path
    )
    assert outcome.admitted is False
    assert "executes repository source beyond importing its module" in outcome.reason


def test_evaluate_refuses_stale_torch_pin(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    [outcome] = _evaluate(
        monkeypatch,
        declarations=[_declaration(pinned={"torch": "2.12.0+cu126"})],
        measure=_never_measure(),
        installed_torch=TORCH_VERSION,
        tmp_path=tmp_path,
    )
    assert outcome.admitted is False
    assert "2.12.0+cu126" in outcome.reason
    assert TORCH_VERSION in outcome.reason
    assert "re-verify the invariant and re-pin" in outcome.reason


def test_evaluate_refuses_empty_justification(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    [outcome] = _evaluate(
        monkeypatch,
        declarations=[_declaration(justification="")],
        measure=_never_measure(),
        tmp_path=tmp_path,
    )
    assert outcome.admitted is False
    assert outcome.reason == "the justification is empty"


def test_evaluate_refuses_whitespace_only_justification(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    [outcome] = _evaluate(
        monkeypatch,
        declarations=[_declaration(justification="   \n\t  ")],
        measure=_never_measure(),
        tmp_path=tmp_path,
    )
    assert outcome.admitted is False
    assert outcome.reason == "the justification is empty"


def test_evaluate_refuses_missing_pinned_mapping(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    declaration = {"nodeid": NODEID, "justification": "why this holds"}
    [outcome] = _evaluate(
        monkeypatch,
        declarations=[declaration],
        measure=_never_measure(),
        tmp_path=tmp_path,
    )
    assert outcome.admitted is False
    assert outcome.reason == "no pinned torch version is declared"


def test_evaluate_refuses_pinned_without_torch_key(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    [outcome] = _evaluate(
        monkeypatch,
        declarations=[_declaration(pinned={"torch": 213})],
        measure=_never_measure(),
        tmp_path=tmp_path,
    )
    assert outcome.admitted is False
    assert outcome.reason == "no pinned torch version is declared"


def test_evaluate_ignores_declaration_not_in_gated_nodeids(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    other = "research/tests/test_x.py::test_not_gated_this_run"
    outcomes = _evaluate(
        monkeypatch,
        declarations=[_declaration(nodeid=other)],
        measure=_never_measure(),
        gated_nodeids=GATED,
        tmp_path=tmp_path,
    )
    assert outcomes == []


def test_evaluate_ignores_declaration_with_non_string_nodeid(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    # A crafted gated set containing the same non-string sentinel: only the
    # isinstance guard stops it from being treated as "gated".
    outcomes = _evaluate(
        monkeypatch,
        declarations=[_declaration(nodeid=1234)],
        measure=_never_measure(),
        gated_nodeids=(*GATED, 1234),  # type: ignore[arg-type]
        tmp_path=tmp_path,
    )
    assert outcomes == []


def test_evaluate_refuses_when_measurement_reports_explicit_error(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    measure = _stub_measure(
        {"import": (frozenset(), "import run failed under coverage: boom")}
    )
    [outcome] = _evaluate(
        monkeypatch, declarations=[_declaration()], measure=measure, tmp_path=tmp_path
    )
    assert outcome.admitted is False
    assert outcome.reason == "import run failed under coverage: boom"


def test_evaluate_refuses_when_measurement_raises_timeout(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    measure = _raising_measure(subprocess.TimeoutExpired(cmd="pytest", timeout=300))
    [outcome] = _evaluate(
        monkeypatch, declarations=[_declaration()], measure=measure, tmp_path=tmp_path
    )
    assert outcome.admitted is False
    assert "coverage could not be measured" in outcome.reason


def test_evaluate_refuses_when_torch_is_not_installed(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    [outcome] = _evaluate(
        monkeypatch,
        declarations=[_declaration()],
        measure=_never_measure(),
        installed_torch=None,
        tmp_path=tmp_path,
    )
    assert outcome.admitted is False
    assert outcome.reason == "torch is not installed, so the pin cannot be checked"
