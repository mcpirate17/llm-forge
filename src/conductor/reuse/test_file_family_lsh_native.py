from __future__ import annotations

import hashlib
import itertools
import random
from collections import defaultdict
from pathlib import Path

import pytest

from conductor.reuse import file_families

PRIME = (1 << 61) - 1
MAX_BUCKET_SIZE = 80


def _profile(index: int, features: set[str]) -> file_families.FileProfile:
    empty = frozenset()
    return file_families.FileProfile(
        file=f"pkg/profile_{index}.py",
        loc=100,
        classes=empty,
        function_names=empty,
        method_names=empty,
        method_hashes={},
        api=empty,
        fields=empty,
        calls=empty,
        control=empty,
        imports=empty,
        structure=frozenset(features),
    )


def _feature_hash(feature: str) -> int:
    digest = hashlib.blake2b(feature.encode("utf-8"), digest_size=8).digest()
    return int.from_bytes(digest, "big") & PRIME


def _reference_pairs(
    profiles: list[file_families.FileProfile],
    *,
    permutations: int,
    band_size: int,
) -> set[tuple[int, int]]:
    if permutations % band_size:
        raise ValueError("num_permutations must be divisible by band_size")
    buckets: dict[tuple[int, tuple[int, ...]], list[int]] = defaultdict(list)
    for profile_index, profile in enumerate(profiles):
        values = [_feature_hash(feature) for feature in profile.lsh_features]
        signature = []
        for index in range(permutations):
            multiplier = (0x9E3779B185EBCA87 + 2 * index) % PRIME or 1
            offset = (0xC2B2AE3D27D4EB4F * (index + 1)) % PRIME
            signature.append(
                min((multiplier * value + offset) % PRIME for value in values)
            )
        for start in range(0, permutations, band_size):
            band = start // band_size
            buckets[(band, tuple(signature[start : start + band_size]))].append(
                profile_index
            )
    pairs: set[tuple[int, int]] = set()
    for members in buckets.values():
        if len(members) < 2 or len(members) > MAX_BUCKET_SIZE:
            continue
        pairs.update(tuple(sorted(pair)) for pair in itertools.combinations(members, 2))
    return pairs


def _random_profiles() -> list[file_families.FileProfile]:
    seed = 20260903
    rng = random.Random(seed)
    profiles = []
    for index in range(72):
        cluster = index // 12
        shared = {f"cluster:{cluster}:{slot}" for slot in range(28)}
        sampled = set(rng.sample(sorted(shared), 19))
        noise = {f"noise:{slot}" for slot in rng.sample(range(600), 9)}
        profiles.append(_profile(index, sampled | noise | {f"unique:{index}"}))
    return profiles


def _unoriented(pairs: set[tuple[int, int]]) -> set[frozenset[int]]:
    return {frozenset(pair) for pair in pairs}


def test_native_lsh_pairs_match_independent_reference() -> None:
    profiles = _random_profiles()
    assert file_families._feature_hash("mask-contract") == 1601420257758367900
    for permutations, band_size in ((12, 3), (24, 4), (48, 4), (48, 8)):
        expected = _reference_pairs(
            profiles, permutations=permutations, band_size=band_size
        )
        actual = file_families.candidate_pairs(
            profiles, permutations=permutations, band_size=band_size
        )
        assert _unoriented(actual) == _unoriented(expected)


def test_native_lsh_pairs_preserve_bucket_and_argument_boundaries() -> None:
    identical = {"shared:a", "shared:b", "shared:c", "shared:d"}
    profiles_80 = [_profile(index, identical) for index in range(80)]
    profiles_81 = [_profile(index, identical) for index in range(81)]
    assert _unoriented(
        file_families.candidate_pairs(profiles_80, permutations=8, band_size=2)
    ) == _unoriented(set(itertools.combinations(range(80), 2)))
    assert not file_families.candidate_pairs(profiles_81, permutations=8, band_size=2)

    empty = [_profile(0, set()), _profile(1, identical)]
    with pytest.raises(ValueError, match=r"min\(\) arg is an empty sequence"):
        file_families.candidate_pairs(empty, permutations=8, band_size=2)
    assert not file_families.candidate_pairs(empty, permutations=0, band_size=2)
    negative = [_profile(index, set()) for index in range(4)]
    assert _unoriented(
        file_families.candidate_pairs(negative, permutations=-4, band_size=-2)
    ) == _unoriented(set(itertools.combinations(range(4), 2)))
    with pytest.raises(ValueError, match="num_permutations"):
        file_families.candidate_pairs(profiles_80, permutations=7, band_size=2)
    with pytest.raises(ZeroDivisionError):
        file_families.candidate_pairs(profiles_80, permutations=8, band_size=0)


def test_scan_file_families_reports_native_lsh_telemetry(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    profiles = _random_profiles()[:24]
    lsh_pairs = _reference_pairs(profiles, permutations=12, band_size=3)
    exact_pairs = set(sorted(lsh_pairs)[:4])
    assert len(exact_pairs) == 4
    monkeypatch.setattr(
        file_families,
        "collect_profiles",
        lambda repo, targets, exclude, min_file_loc: (profiles, 2),
    )
    monkeypatch.setattr(
        file_families,
        "exact_candidate_pairs",
        lambda profiles, **kwargs: (exact_pairs, 276),
    )
    monkeypatch.setattr(
        file_families,
        "build_families",
        lambda profiles, pairs, **kwargs: ([], 4),
    )

    families, stats = file_families.scan_file_families(
        Path("."), ["pkg"], set(), permutations=12, band_size=3
    )
    assert families == []
    assert stats == {
        "files_profiled": 24,
        "files_unparsable": 2,
        "pair_universe": 276,
        "exact_candidate_pairs": 4,
        "pairs_pruned_by_safe_bound": 272,
        "lsh_candidate_pairs": len(lsh_pairs),
        "lsh_recall": round(len(lsh_pairs & exact_pairs) / 4, 4),
        "pairs_scored": 4,
        "families": 0,
        "estimated_net_deleted_loc": 0,
    }
