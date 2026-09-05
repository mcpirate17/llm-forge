"""Tests for the uncurated-mutant generator.

The generator's output is a claim about test strength, so the properties that
matter are the ones that would silently inflate or deflate that claim: mutating a
line nobody executes (a coverage gap reported as a detection gap), emitting a
mutant identical to the original (a free survivor), emitting one that does not
compile (a free kill), or changing more than one site at a time (an unattributable
verdict). Each is pinned below.
"""

from __future__ import annotations

import ast
import json
import pathlib
import sys

import pytest

from conductor.uncurated_kill_rate import (
    CampaignLookupError,
    generate,
    main,
    measure,
    resolve_campaigns,
)


def _all(source: str) -> set[int]:
    """Every line of `source`, i.e. "the tests executed all of it"."""

    return set(range(1, len(source.splitlines()) + 1))


def _sources(source: str, covered: set[int] | None = None) -> list[str]:
    lines = _all(source) if covered is None else covered
    return [m.source for m in generate(source, "subject.py", lines)]


def _details(source: str, covered: set[int] | None = None) -> set[str]:
    lines = _all(source) if covered is None else covered
    return {m.detail for m in generate(source, "subject.py", lines)}


def test_a_comparison_is_flipped_to_its_boundary_neighbour() -> None:
    out = _sources("def f(n):\n    return n < 1\n")
    assert any("n <= 1" in s for s in out)


def test_membership_and_identity_comparisons_are_flipped() -> None:
    details = _details("def f(a, b):\n    return a in b\n")
    assert "In->NotIn" in details
    details = _details("def f(a, b):\n    return a is b\n")
    assert "Is->IsNot" in details


def test_a_boolean_operator_is_flipped() -> None:
    out = _sources("def f(a, b):\n    return a and b\n")
    assert any("a or b" in s for s in out)


def test_a_negation_is_dropped() -> None:
    out = _sources("def f(a):\n    return not a\n")
    assert any(s.strip().endswith("return a") for s in out)


def test_arithmetic_is_swapped() -> None:
    assert "Add->Sub" in _details("def f(a, b):\n    return a + b\n")


def test_an_augmented_assignment_is_swapped() -> None:
    assert "Add->Sub" in _details("def f(a):\n    a += 1\n    return a\n")


def test_a_boolean_constant_is_inverted() -> None:
    out = _sources("def f():\n    return True\n")
    assert any("return False" in s for s in out)


def test_an_integer_constant_is_perturbed() -> None:
    out = _sources("def f():\n    return 7\n")
    assert any("return 8" in s for s in out)


def test_a_string_constant_is_emptied() -> None:
    out = _sources("def f():\n    return 'abc'\n")
    assert any("return ''" in s for s in out)


def test_a_returned_value_is_replaced_with_none() -> None:
    out = _sources("def f(a):\n    return a\n")
    # `ast.unparse` renders `return None` as a bare `return` -- same semantics
    assert any(s.splitlines()[-1].strip() == "return" for s in out)
    # `return None` is already None, so it is not a mutation site
    assert not [
        m
        for m in generate("def f():\n    return None\n", "s.py", {1, 2})
        if m.op == "return-none"
    ]


def test_loop_exits_are_exchanged() -> None:
    source = "def f(xs):\n    for x in xs:\n        if x:\n            continue\n        break\n"
    details = _details(source)
    assert "continue -> break" in details
    assert "break -> continue" in details


def test_an_uncovered_line_is_never_mutated() -> None:
    source = "def f(a, b):\n    if a < b:\n        return 1\n    return 2\n"
    assert _sources(source, covered=set()) == []
    only_guard = generate(source, "subject.py", {2})
    assert only_guard and {m.line for m in only_guard} == {2}


def test_output_is_well_formed_and_free_of_duplicates() -> None:
    """A repeat inflates the total; an echo of the original is a free survivor."""

    # `not not a` carries two `not`-removal sites that unparse to the same text, so
    # a generator that does not de-duplicate reports one mutant twice.
    source = (
        "def f(a, b):\n"
        "    if a < b and not not a:\n"
        "        return 'x'\n"
        "    return a + 1\n"
    )
    original = ast.unparse(ast.parse(source))
    mutants = _sources(source)
    assert mutants
    assert original not in mutants
    assert len(set(mutants)) == len(mutants)
    for text in mutants:
        compile(text, "subject.py", "exec")


def test_the_operator_label_names_what_changed() -> None:
    """A verdict is filed under its operator, so a wrong label misreports the run."""

    compare = generate("def f(a, b):\n    if a < b:\n        pass\n", "s.py", {1, 2, 3})
    assert {m.op for m in compare} == {"compare"}
    constant = generate("def f():\n    x = True\n", "s.py", {1, 2})
    assert {m.op for m in constant} == {"const-bool"}


def test_each_mutant_changes_exactly_one_site() -> None:
    """Two changes at once would make a verdict unattributable to either."""

    # The sites are spread over separate lines so that a mutant touching more than
    # one of them shows up as more than one differing line.
    source = "def f(a, b):\n    if a < b:\n        return 1\n    return a + 1\n"
    baseline = ast.unparse(ast.parse(source)).splitlines()
    mutants = _sources(source)
    assert mutants
    for text in mutants:
        lines = text.splitlines()
        assert len(lines) == len(baseline)
        differing = [
            i for i, (x, y) in enumerate(zip(baseline, lines, strict=True)) if x != y
        ]
        assert len(differing) == 1, (differing, text)


# --------------------------------------------------------------- end to end


SUBJECT = """LIMIT = 10
MESSAGE = 'unread'


def classify(n):
    if n < LIMIT:
        return 'small'
    return 'large'
"""

SUBJECT_TESTS = """from subject import classify


def test_below_the_limit():
    assert classify(1) == 'small'


def test_at_the_limit():
    assert classify(10) == 'large'
"""


def _fixture(root: pathlib.Path, subjects: dict[str, str]) -> pathlib.Path:
    """A minimal campaign tree: subject, tests, and a manifest measure() can read."""

    (root / "subject.py").write_text(SUBJECT)
    (root / "test_subject.py").write_text(SUBJECT_TESTS)
    manifest = root / "campaign.json"
    manifest.write_text(
        json.dumps(
            {
                "source_sha256": subjects,
                "baseline": {
                    "argv": [
                        "python",
                        "-m",
                        "pytest",
                        "-q",
                        "-o",
                        "addopts=",
                        "--rootdir=.",
                        "test_subject.py",
                    ]
                },
            }
        )
    )
    return manifest


def test_measure_counts_by_exit_code_and_restores_the_tree(
    tmp_path: pathlib.Path,
) -> None:
    """The counts are the published number, and the tree is mutated in place."""

    manifest = _fixture(tmp_path, {"subject.py": "", "test_subject.py": ""})
    res = measure(tmp_path, manifest, cap=50, seed=0, budget=300.0)

    assert res.ran == res.killed + res.survived + res.timeout + res.broken
    assert res.broken == 0
    # `MESSAGE` is executed on import and read by nothing the tests assert on:
    # the covered-but-unchecked line this harness exists to find.
    assert res.survivors == [
        {"path": "subject.py", "line": 2, "op": "const-str", "detail": "'unread'->''"}
    ]
    assert res.killed >= 4
    assert (tmp_path / "subject.py").read_text() == SUBJECT


def test_a_test_file_is_never_a_mutation_subject(tmp_path: pathlib.Path) -> None:
    """Mutating the tests would score the harness against itself."""

    manifest = _fixture(tmp_path, {"test_subject.py": ""})
    res = measure(tmp_path, manifest, cap=50, seed=0, budget=300.0)

    assert (res.generated, res.ran, res.baseline_seconds) == (0, 0, 0.0)


def test_the_cli_writes_a_row_for_every_campaign(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A campaign with no row is a campaign whose number was never published."""

    campaigns = tmp_path / "conductor" / "mutation_campaigns"
    campaigns.mkdir(parents=True)
    manifest = _fixture(tmp_path, {"test_subject.py": ""})
    for name in ("alpha", "beta"):
        (campaigns / f"{name}.json").write_text(manifest.read_text())
    out = tmp_path / "results.json"
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "uncurated_kill_rate",
            "--root",
            str(tmp_path),
            "--campaign",
            "alpha",
            "--campaign",
            "beta",
            "--out",
            str(out),
        ],
    )

    assert main() == 0
    rows = json.loads(out.read_text())
    assert [row["campaign"] for row in rows] == ["alpha", "beta"]


def test_a_campaign_resolves_by_the_id_its_manifest_declares(
    tmp_path: pathlib.Path,
) -> None:
    """`mutation_value_self_v4` lives in `mutation_value_self.json`."""

    campaigns = tmp_path / "conductor" / "mutation_campaigns"
    campaigns.mkdir(parents=True)
    manifest = _fixture(tmp_path, {"test_subject.py": ""})
    payload = json.loads(manifest.read_text())
    payload["campaign_id"] = "declared_id_v4"
    (campaigns / "on_disk_name.json").write_text(json.dumps(payload))

    assert resolve_campaigns(tmp_path, ["declared_id_v4"]) == [
        ("declared_id_v4", campaigns / "on_disk_name.json")
    ]


def test_an_unresolvable_campaign_is_refused_before_anything_is_measured(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A typo used to abort mid-batch, after the machine time was already spent."""

    campaigns = tmp_path / "conductor" / "mutation_campaigns"
    campaigns.mkdir(parents=True)
    manifest = _fixture(tmp_path, {"subject.py": "", "test_subject.py": ""})
    (campaigns / "alpha.json").write_text(manifest.read_text())
    out = tmp_path / "results.json"
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "uncurated_kill_rate",
            "--root",
            str(tmp_path),
            "--campaign",
            "alpha",
            "--campaign",
            "nope",
            "--campaign",
            "also_nope",
            "--out",
            str(out),
        ],
    )

    with pytest.raises(CampaignLookupError) as excinfo:
        main()

    # Every unresolvable id is named, not just the first one hit.
    assert "nope" in str(excinfo.value)
    assert "also_nope" in str(excinfo.value)
    # And `alpha`, which does resolve, was not measured on the way to the refusal.
    assert not out.exists()


def test_a_campaign_that_executes_no_subject_is_a_row_not_a_crash(
    tmp_path: pathlib.Path,
) -> None:
    """`coverage json` exits 1 on an empty data file.

    That killed a 17-campaign batch at campaign seven, after the machine time for
    six had been spent. An unmeasurable campaign is a row carrying its reason.
    """

    (tmp_path / "subject.py").write_text("def unused():\n    return 1\n")
    (tmp_path / "test_subject.py").write_text("def test_nothing():\n    assert True\n")
    manifest = tmp_path / "campaign.json"
    manifest.write_text(
        json.dumps(
            {
                "source_sha256": {"subject.py": "", "test_subject.py": ""},
                "baseline": {
                    "argv": [
                        "python",
                        "-m",
                        "pytest",
                        "-q",
                        "-o",
                        "addopts=",
                        "--rootdir=.",
                        "test_subject.py",
                    ]
                },
            }
        )
    )

    res = measure(tmp_path, manifest, cap=50, seed=0, budget=300.0)

    assert (res.ran, res.generated) == (0, 0)
    assert res.note == "coverage measurement failed"
