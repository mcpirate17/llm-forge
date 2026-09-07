from __future__ import annotations

import hashlib
from pathlib import Path

import pytest

from conductor.reuse import consolidation, detectors, file_families


def _candidate(identifier: str, value: int, confidence: float, **extra: object) -> dict:
    return {
        "id": identifier,
        "value": value,
        "confidence": confidence,
        **extra,
    }


def _inventory(repo: Path, **overrides: object) -> dict[str, list[dict]]:
    arguments: dict[str, object] = {
        "god_files": [],
        "god_functions": [],
        "clusters": [],
        "families": [],
        "vulture": [],
        "ruff": [],
        "fallbacks": [],
        "token_clones": [],
        "native_reuse": [],
        "dependencies": [],
        "compliance": [],
        "contract_candidates": [],
        "limit": 80,
        "test_limit": 80,
    }
    arguments.update(overrides)
    return detectors._inventory_candidates(repo, **arguments)


def _cluster(sites: list[consolidation.FuncRecord]) -> consolidation.Cluster:
    return consolidation.Cluster(
        kind="exact",
        tokens=20,
        sites=sites,
        confidence=0.75,
        value_score=40,
        disposition="auto",
        rationale="same normalized body",
    )


def _site(file: str, line: int, name: str) -> consolidation.FuncRecord:
    return consolidation.FuncRecord(file, line, line + 4, name, "hash", 10, "source")


def test_policy_thresholds_remain_python_owned(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(detectors, "GOD_FILE_LINES", 10)
    monkeypatch.setattr(detectors, "GOD_FUNC_LINES", 5)

    result = _inventory(
        tmp_path,
        god_files=[f"{tmp_path / 'large.py'} (17)"],
        god_functions=[f"{tmp_path / 'large.py'}:3 oversized (11)"],
    )

    assert result["god_files"][0]["value"] == 7
    assert result["god_files"][0]["evidence"] == "17 lines; threshold is 10"
    assert result["god_functions"][0]["value"] == 6
    assert result["god_functions"][0]["evidence"].endswith("threshold is 5")


def test_clone_identity_is_order_independent_but_location_keeps_input_order(
    tmp_path: Path,
) -> None:
    left = _site("pkg/z.py", 20, "z")
    right = _site("pkg/a.py", 10, "a")
    forward = _inventory(tmp_path, clusters=[_cluster([left, right])])["duplication"][0]
    reverse = _inventory(tmp_path, clusters=[_cluster([right, left])])["duplication"][0]
    identity = "\0".join(sorted(("pkg/z.py:20-24:z", "pkg/a.py:10-14:a")))
    expected_id = hashlib.sha256(identity.encode()).hexdigest()[:20]

    assert forward["id"] == reverse["id"] == f"reuse:{expected_id}"
    assert forward["files"] == ["pkg/a.py", "pkg/z.py"]
    assert forward["location"] == "pkg/z.py:20, pkg/a.py:10"
    assert reverse["location"] == "pkg/a.py:10, pkg/z.py:20"


def test_ranking_limits_and_pass_through_identity_match_python(
    tmp_path: Path,
) -> None:
    fallback_low = _candidate("fallback-low", 1, 0.1)
    fallback_high = _candidate("fallback-high", 9, 0.9)
    native_incomplete = _candidate(
        "native-incomplete", 20, 0.9, evidence_complete=False
    )
    native_complete = _candidate("native-complete", 10, 0.8, evidence_complete=1)
    native_complete_low = _candidate(
        "native-complete-low", 2, 0.7, evidence_complete="yes"
    )
    native_missing = _candidate("native-missing", 5, 1.0)
    token_candidates = [
        _candidate("token-low", 1, 0.9),
        _candidate("token-middle", 2, 0.1),
        _candidate("token-high", 3, 0.5),
    ]
    fallbacks = [fallback_low, fallback_high]
    native = [native_complete, native_missing, native_complete_low, native_incomplete]

    result = _inventory(
        tmp_path,
        fallbacks=fallbacks,
        token_clones=token_candidates,
        native_reuse=native,
        limit=-1,
    )

    assert result["duplication"] == [token_candidates[2], token_candidates[1]]
    assert result["silent_fallbacks"] == [fallback_high]
    assert result["silent_fallbacks"][0] is fallback_high
    assert fallbacks == [fallback_high, fallback_low]
    assert result["perf_hotspots"] == [native_complete]
    assert result["perf_hotspots"][0] is native_complete
    assert result["native_reuse"] == [
        native_incomplete,
        native_complete,
        native_missing,
    ]
    assert native == [
        native_incomplete,
        native_complete,
        native_missing,
        native_complete_low,
    ]


def test_ties_duplicates_and_zero_limit_preserve_python_slice_contract(
    tmp_path: Path,
) -> None:
    first = _candidate("first", 7, 0.5, extra="preserved")
    second = _candidate("second", 7, 0.5)
    confidence_winner = _candidate("confidence-winner", 7, 0.9)
    duplicate = _candidate("duplicate", 3, 0.4)
    ordered = [first, second, confidence_winner, duplicate, duplicate]

    positive = _inventory(tmp_path, token_clones=ordered, limit=5)["duplication"]
    empty = _inventory(tmp_path, token_clones=ordered, limit=0)["duplication"]

    assert positive == [confidence_winner, first, second, duplicate, duplicate]
    assert positive[0] is confidence_winner
    assert positive[1] is first
    assert positive[2] is second
    assert positive[3] is positive[4] is duplicate
    assert empty == []


def test_all_constructed_categories_keep_exact_output_contract(tmp_path: Path) -> None:
    family = file_families.FileFamily(
        files=["pkg/a.py", "pkg/b.py"],
        similarity_min=0.75,
        similarity_avg=0.812,
        similarity_max=0.9,
        containment_min=0.8,
        shared_methods=["shared"],
        variable_methods=["vary"],
        common_fields=["field"],
        recommended_abstraction="base-class",
        suggested_home=None,
        gross_duplicate_loc=400,
        estimated_net_deleted_loc=220,
        confidence=0.85,
        risk="medium",
        band="strong",
        disposition="validate",
        before_loc=500,
        after_loc=280,
        target_shape="base+config",
    )
    dependency = _candidate("dependency", 12, 0.6)
    compliance = _candidate("compliance", 4, 0.5)
    contract = _candidate("contract", 8, 0.7)

    result = _inventory(
        tmp_path,
        families=[family],
        vulture=[f"{tmp_path / 'dead.py'}:7: unused helper (75% confidence)"],
        ruff=[
            {
                "filename": str(tmp_path / "lint.py"),
                "location": {"row": 9},
                "code": "F401",
                "message": "unused import",
                "extra": "ignored",
            },
            {
                "filename": str(tmp_path / "nullable.py"),
                "location": {"row": None},
                "code": None,
                "message": None,
            },
        ],
        dependencies=[dependency],
        compliance=[compliance],
        contract_candidates=[contract],
    )

    assert result["dead_code"] == [
        {
            "id": "dead:dead.py:7",
            "category": "dead_code",
            "severity": "medium",
            "confidence": 0.75,
            "value": 30,
            "files": ["dead.py"],
            "location": "dead.py:7",
            "evidence": "unused helper",
        }
    ]
    assert result["imports_deps"][0]["id"] == "dependency"
    assert result["imports_deps"][0] is dependency
    assert result["imports_deps"][1]["id"] == "ruff:F401:lint.py:9"
    assert result["imports_deps"][2]["id"] == "ruff:None:nullable.py:None"
    assert result["file_families"][0] == {
        "id": f"family:{hashlib.sha256(b'pkg/a.py\0pkg/b.py').hexdigest()[:20]}",
        "category": "file_families",
        "severity": "high",
        "confidence": 0.85,
        "value": 220,
        "files": ["pkg/a.py", "pkg/b.py"],
        "location": "pkg/a.py, pkg/b.py",
        "evidence": (
            "strong family; min/avg similarity 75.0%/81.2%; containment>=80.0%; "
            "recommend=base-class; before_loc=500; after_loc=280; "
            "target_shape=base+config; net_deleted_loc=220; "
            "shared_methods=['shared']; variable_methods=['vary']; "
            "suggested_home=leader-selected"
        ),
        "before_loc": 500,
        "after_loc": 280,
        "target_shape": "base+config",
    }
    assert result["compliance"] == [compliance]
    assert result["test_contracts"] == [contract]


def test_malformed_optional_measurements_are_skipped_but_required_ones_fail(
    tmp_path: Path,
) -> None:
    result = _inventory(
        tmp_path,
        god_functions=["not a function measurement"],
        vulture=["not a vulture measurement"],
    )
    assert result["god_functions"] == []
    assert result["dead_code"] == []

    with pytest.raises(ValueError, match="malformed god-file measurement"):
        _inventory(tmp_path, god_files=["not a file measurement"])
