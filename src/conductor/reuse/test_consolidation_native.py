from __future__ import annotations

from conductor.reuse import consolidation as native


def _records() -> list[native.FuncRecord]:
    records = []
    specifications = (
        ("exact-local", "pkg/local", "shared", "return value", 60, 3),
        ("exact-local-two", "pkg/local", "shared_two", "return other", 55, 2),
        ("near-local", "pkg/near", "renamed", None, 48, 3),
        ("exact-risk", "research/synthesis", "tpl_lane", "return x", 90, 2),
        ("cross-root", None, "helper", "return item", 72, 2),
    )
    for group, directory, name, shared_source, tokens, count in specifications:
        for member in range(count):
            root = directory or ("pkg" if member == 0 else "research")
            source = shared_source or f"return renamed_{member}"
            records.append(
                native.FuncRecord(
                    file=f"{root}/member_{member}.py",
                    line_start=10 + member,
                    line_end=20 + member,
                    name=name if member == 0 else f"_{name}",
                    node_hash=group,
                    tokens=tokens,
                    source=source,
                )
            )
    # Python's dict comprehension keeps the first key position but the last record.
    records.append(
        native.FuncRecord(
            file="pkg/local/member_0.py",
            line_start=10,
            line_end=25,
            name="shared",
            node_hash="exact-local",
            tokens=60,
            source="return value",
        )
    )
    return records


def _summary(clusters: list[native.Cluster]) -> list[tuple]:
    return [
        (
            cluster.kind,
            cluster.tokens,
            [
                (site.file, site.line_start, site.line_end, site.name, site.source)
                for site in cluster.sites
            ],
            cluster.confidence,
            cluster.value_score,
            cluster.risk,
            cluster.disposition,
            cluster.rationale,
        )
        for cluster in clusters
    ]


def test_native_cluster_aggregation_matches_python() -> None:
    clusters = native.build_clusters(_records())
    assert _summary(clusters) == [
        (
            "exact",
            60,
            [
                ("pkg/local/member_0.py", 10, 25, "shared", "return value"),
                ("pkg/local/member_1.py", 11, 21, "_shared", "return value"),
                ("pkg/local/member_2.py", 12, 22, "_shared", "return value"),
            ],
            0.99,
            63,
            "low",
            "auto",
            "exact, same-dir",
        ),
        (
            "near",
            48,
            [
                ("pkg/near/member_0.py", 10, 20, "renamed", "return renamed_0"),
                ("pkg/near/member_1.py", 11, 21, "_renamed", "return renamed_1"),
                ("pkg/near/member_2.py", 12, 22, "_renamed", "return renamed_2"),
            ],
            0.9,
            46,
            "low",
            "auto",
            "near, same-dir",
        ),
        (
            "exact",
            55,
            [
                ("pkg/local/member_0.py", 10, 20, "shared_two", "return other"),
                ("pkg/local/member_1.py", 11, 21, "_shared_two", "return other"),
            ],
            0.99,
            35,
            "low",
            "auto",
            "exact, same-dir",
        ),
        (
            "exact",
            90,
            [
                ("research/synthesis/member_0.py", 10, 20, "tpl_lane", "return x"),
                ("research/synthesis/member_1.py", 11, 21, "_tpl_lane", "return x"),
            ],
            0.71,
            10,
            "high",
            "ignore",
            "exact, same-dir, semantic-family-risk",
        ),
        (
            "exact",
            72,
            [
                ("pkg/member_0.py", 10, 20, "helper", "return item"),
                ("research/member_1.py", 11, 21, "_helper", "return item"),
            ],
            0.81,
            4,
            "high",
            "ignore",
            "exact, cross-dir",
        ),
    ]
    native._assign_ids(clusters, batch_size=2)
    assert [
        (cluster.id, cluster.batch, native._suggested_home(cluster.sites))
        for cluster in clusters
    ] == [
        ("C001", 0, "pkg/local/_member.py"),
        ("C002", 1, "pkg/near/_member.py"),
        ("C003", 0, "pkg/local/_member.py"),
        ("C004", -1, "research/synthesis/_member.py"),
        ("C005", -1, None),
    ]


def test_native_token_clone_evidence_matches_python() -> None:
    report = {
        "duplicates": [
            {
                "tokens": 80,
                "firstFile": {"name": "pkg/one.py", "start": 3, "end": 20},
                "secondFile": {"name": "pkg/two.py", "start": 4, "end": 21},
            },
            {"tokens": 10, "firstFile": {}, "secondFile": {}},
        ]
    }
    actual = native._clusters_from_jscpd(report)
    assert _summary(actual) == [
        (
            "token",
            80,
            [
                ("pkg/one.py", 3, 20, "<clone>", ""),
                ("pkg/two.py", 4, 21, "<clone>", ""),
            ],
            0.9,
            47,
            "low",
            "auto",
            "token, same-dir",
        )
    ]
