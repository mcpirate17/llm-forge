"""A policy exception that excuses nothing is debt, and the review says so.

An exemption stops being governance the moment the finding it covers is gone: the
code was fixed, or a pinned fingerprint drifted, and what remains is a line that
reads as a considered decision while doing nothing. `mut-testing-oversized-func`
outlived its finding by weeks -- the function it excused was 46 lines by the time
anyone looked.

The discrimination that makes this reportable rather than noisy is that an
exception is judged only against the files its own check actually read. Nothing
is stale merely because this candidate did not touch it.
"""

from __future__ import annotations

from datetime import date, timedelta
from pathlib import Path

from conductor.candidate_review.engine import examined_paths
from conductor.candidate_review.model import (
    CheckResult,
    CheckStatus,
    Finding,
    ReviewReceipt,
    Severity,
    seal_receipt,
)
from conductor.candidate_review.policy import Policy, load_policy, unmatched_exceptions
from conductor.candidate_review.reporters import human_summary

CHECK_BLOCK = """\
[checks.{check}]
kind = "builtin"
profiles = ["fast", "full"]
classes = []
severity = "critical"
always = true
cache = false
run_on_deletions = true
timeout_seconds = 10
memory_mb = 128
max_output_chars = 1000
"""


def policy_with(tmp_path: Path, exceptions: str) -> Policy:
    """A real policy, loaded the way the gate loads one."""
    horizon = (date.today() + timedelta(days=30)).isoformat()
    path = tmp_path / "candidate_policy.toml"
    path.write_text(
        f"""\
schema_version = 1
block_at = "high"
max_workers = 1
cache_ttl_days = 1
claim_max_age_hours = 1
max_file_bytes = 1000000
max_binary_bytes = 1000000
coverage_threshold = 75.0
high_risk_coverage_threshold = 90.0
baseline_expires = {horizon}
exceptions = [
{exceptions}
]

[classes]

[risk]
high = []

[paths]
protected_deletes = []
hot = []
generated = []

{CHECK_BLOCK.format(check="candidate-integrity")}
{CHECK_BLOCK.format(check="secret-scan")}
""",
        encoding="utf-8",
    )
    return load_policy(path)


def exception_entry(
    identifier: str,
    *,
    check: str = "candidate-integrity",
    path: str = "conductor/target.py",
    rule: str | None = None,
    owner: str = "claude",
) -> str:
    horizon = (date.today() + timedelta(days=30)).isoformat()
    rule_field = f'rule = "{rule}", ' if rule else ""
    return (
        f'  {{ id = "{identifier}", check = "{check}", {rule_field}'
        f'path = "{path}", owner = "{owner}", '
        f'justification = "covers a finding that may no longer exist", '
        f"expires = {horizon} }},"
    )


def ids(stale: tuple[dict[str, str], ...]) -> list[str]:
    return [entry["id"] for entry in stale]


def test_an_exception_is_stale_only_when_its_own_check_read_its_file(tmp_path: Path):
    policy = policy_with(
        tmp_path,
        "\n".join(
            [
                exception_entry("read-and-excused-nothing"),
                exception_entry("check-read-other-files", path="conductor/other.py"),
                exception_entry("another-checks-exemption", check="secret-scan"),
            ]
        ),
    )
    # secret-scan never ran, so its exemption on the very file candidate-integrity
    # read is not judged: only the check that owns an exception can retire it.
    examined = {"candidate-integrity": {"conductor/target.py"}}
    assert ids(unmatched_exceptions(policy, examined, [])) == [
        "read-and-excused-nothing"
    ]


def test_an_exception_that_excused_a_finding_is_not_stale(tmp_path: Path):
    policy = policy_with(tmp_path, exception_entry("live"))
    finding = Finding(
        check_id="candidate-integrity",
        rule_id="unsafe-symlink",
        severity=Severity.CRITICAL,
        message="candidate symlink escapes its snapshot",
        path="conductor/target.py",
        exception_id="live",
    )
    examined = {"candidate-integrity": {"conductor/target.py"}}
    assert unmatched_exceptions(policy, examined, [finding]) == ()


def test_a_path_glob_is_matched_against_what_the_check_read(tmp_path: Path):
    policy = policy_with(
        tmp_path,
        exception_entry("globbed", path="conductor/candidate_review/mutation_*.py"),
    )
    examined = {
        "candidate-integrity": {"conductor/candidate_review/mutation_testing.py"}
    }
    assert ids(unmatched_exceptions(policy, examined, [])) == ["globbed"]


def test_the_report_names_the_owner_the_check_and_the_expiry(tmp_path: Path):
    horizon = (date.today() + timedelta(days=30)).isoformat()
    policy = policy_with(tmp_path, exception_entry("unruled", owner="grok"))
    examined = {"candidate-integrity": {"conductor/target.py"}}
    assert unmatched_exceptions(policy, examined, []) == (
        {
            "id": "unruled",
            "check": "candidate-integrity",
            "rule": "",
            "path": "conductor/target.py",
            "owner": "grok",
            "expires": horizon,
        },
    )


def test_an_exception_pinned_to_a_rule_reports_that_rule(tmp_path: Path):
    policy = policy_with(tmp_path, exception_entry("ruled", rule="oversized-function"))
    examined = {"candidate-integrity": {"conductor/target.py"}}
    assert unmatched_exceptions(policy, examined, [])[0]["rule"] == "oversized-function"


def test_examined_paths_unions_every_result_for_a_check():
    results = [
        CheckResult(
            check_id="python-ast",
            status=CheckStatus.PASSED,
            duration_ms=1,
            files=["conductor/a.py"],
        ),
        CheckResult(
            check_id="python-ast",
            status=CheckStatus.PASSED,
            duration_ms=1,
            files=["conductor/b.py"],
        ),
    ]
    assert examined_paths(results) == {
        "python-ast": {"conductor/a.py", "conductor/b.py"}
    }


STALE = {
    "id": "mut-testing-oversized-func",
    "check": "python-ast",
    "rule": "oversized-function",
    "path": "conductor/mutation_testing.py",
    "owner": "grok",
    "expires": "2026-09-15",
}


def receipt(stale: list[dict[str, str]]) -> ReviewReceipt:
    return seal_receipt(
        ReviewReceipt(
            schema_version=1,
            receipt_id="",
            receipt_digest="",
            surface="local",
            profile="fast",
            decision="pass",
            candidate={"tree_oid": "0" * 40, "changes": []},
            policy={"unmatched_exceptions": stale},
            engine={},
            graph={},
            bypass={},
            timings={"duration_ms": 1},
            cache={"hits": 0},
            baselines=[],
            checks=[],
            findings=[],
            binding="",
        )
    )


def test_the_summary_names_a_stale_exception():
    summary = human_summary(receipt([STALE]))
    assert (
        "- STALE exception mut-testing-oversized-func (grok, expires 2026-09-15): "
        "python-ast read conductor/mutation_testing.py and it excused nothing"
    ) in summary


def test_a_stale_exception_is_not_counted_against_the_candidate():
    summary = human_summary(receipt([STALE]))
    assert "blocking=0 advisory=0 waived=0" in summary
    assert human_summary(receipt([])).count("STALE") == 0
