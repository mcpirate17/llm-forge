from __future__ import annotations

import itertools
import random

from conductor.reuse import file_families as native


def _profiles(seed: int = 20260901) -> list[native.FileProfile]:
    rng = random.Random(seed)
    profiles = []
    for index in range(15):
        cluster = index // 5
        values = {}
        for component in ("structure", "api", "fields", "calls", "control", "imports"):
            shared = {f"{component}:cluster:{cluster}:{slot}" for slot in range(32)}
            noise = {f"{component}:noise:{slot}" for slot in rng.sample(range(100), 5)}
            values[component] = frozenset(shared | noise)
        profiles.append(
            native.FileProfile(
                file=f"pkg/family_{cluster}/member_{index}.py",
                loc=120 + index,
                classes=frozenset({"Lane"}),
                function_names=frozenset({"run"}),
                method_names=frozenset({"forward"}),
                method_hashes={"forward": frozenset({f"hash:{cluster}"})},
                schemas=frozenset(),
                **values,
            )
        )
    return profiles


def test_native_pair_scoring_and_safe_bound_match_python() -> None:
    profiles = _profiles()
    representative = native.compare_profiles(profiles[2], profiles[3])
    assert (
        representative.score,
        representative.containment,
        representative.shared_features,
        representative.components,
    ) == (
        0.7769,
        0.8703,
        161,
        {
            "structure": 0.8049,
            "api": 0.7619,
            "fields": 0.7619,
            "calls": 0.7619,
            "control": 0.7619,
            "imports": 0.7619,
        },
    )
    all_pairs = set(itertools.combinations(range(len(profiles)), 2))
    for threshold in (0.55, 0.70, 0.90):
        kwargs = {"min_similarity": threshold, "min_shared_features": 12}
        assert native.exact_candidate_pairs(profiles, **kwargs) == (all_pairs, 105)


def test_native_complete_link_groups_and_ids_match_python() -> None:
    profiles = _profiles(seed=17)
    pair_kwargs = {"min_similarity": 0.70, "min_shared_features": 12}
    actual_pairs = native.exact_candidate_pairs(profiles, **pair_kwargs)
    family_kwargs = {
        **pair_kwargs,
        "min_net_deleted_loc": 0,
        "max_family_size": 8,
        "max_candidates": 50,
    }
    actual = native.build_families(profiles, actual_pairs[0], **family_kwargs)
    assert actual[1] == 105
    assert [
        (
            family.id,
            family.files,
            family.similarity_min,
            family.similarity_avg,
            family.similarity_max,
            family.containment_min,
            family.gross_duplicate_loc,
            family.estimated_net_deleted_loc,
            family.before_loc,
            family.after_loc,
        )
        for family in actual[0]
    ] == [
        (
            "F001",
            [f"pkg/family_2/member_{index}.py" for index in range(10, 15)],
            0.7619,
            0.7733,
            0.7855,
            0.8649,
            408,
            381,
            660,
            279,
        ),
        (
            "F002",
            [f"pkg/family_1/member_{index}.py" for index in range(5, 10)],
            0.7619,
            0.7688,
            0.7814,
            0.8649,
            392,
            366,
            635,
            269,
        ),
        (
            "F003",
            [f"pkg/family_0/member_{index}.py" for index in range(5)],
            0.7619,
            0.7716,
            0.7837,
            0.8649,
            376,
            350,
            610,
            260,
        ),
    ]
