from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor import mutation_retention
from conductor.mutation_retention import RetentionError


def _write_json(path: Path, payload: object) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload), encoding="utf-8")
    return path


def _campaign(root: Path, campaign_id: str, *, filename: str | None = None) -> Path:
    """Put a campaign on disk the way the gate finds one: a manifest.

    `filename` decouples the manifest's name from its `campaign_id`, which is what
    the gate keys on -- the two agree by convention, not by rule.
    """

    stem = filename or campaign_id
    return _write_json(
        root / f"{mutation_retention.CAMPAIGN_DIRECTORY}/{stem}.json",
        {"campaign_id": campaign_id},
    )


def _receipt(
    root: Path, name: str, campaign_id: str, status: str, at: str, *, pad: int = 0
) -> Path:
    """One receipt. `pad` grows the file without changing what it says, so a test
    can tell the kept bytes apart from the freed ones."""

    path = _write_json(
        root / mutation_retention.RECEIPT_DIRECTORY / f"{name}.json",
        {
            "campaign_id": campaign_id,
            "status": status,
            "generated_at": at,
            "name": name,
        },
    )
    if pad:
        path.write_text(path.read_text(encoding="utf-8") + " " * pad, encoding="utf-8")
    return path


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    (tmp_path / mutation_retention.RECEIPT_DIRECTORY).mkdir(parents=True)
    return tmp_path


def _no_citations(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(mutation_retention, "cited_receipts", lambda repo_root: set())
    _no_audit(monkeypatch)


def _cites(monkeypatch: pytest.MonkeyPatch, *paths: Path) -> None:
    resolved = {path.resolve() for path in paths}
    monkeypatch.setattr(
        mutation_retention, "cited_receipts", lambda repo_root: resolved
    )
    _no_audit(monkeypatch)


def _no_audit(monkeypatch: pytest.MonkeyPatch) -> None:
    """Silence the second authority so a test can isolate the first."""

    monkeypatch.setattr(
        mutation_retention, "audited_receipts", lambda receipts, repo_root: set()
    )


def _audit_accepts(monkeypatch: pytest.MonkeyPatch, *names: str) -> None:
    """Let the real `audited_receipts` run, with the audit's verdict under control.

    Its predicate is patched, not restated: the point of the function is that it
    asks `mutation_patch_audit` rather than reimplementing it, so the test drives
    the same seam production code uses.
    """

    from conductor import mutation_patch_audit

    accepted = set(names)
    monkeypatch.setattr(
        mutation_patch_audit, "_runner_components_sha256", lambda: {"runner": "sha"}
    )
    monkeypatch.setattr(
        mutation_patch_audit,
        "_receipt_rejection",
        lambda receipt, current, repo_root: (
            None if receipt.get("name") in accepted else "rejected by the test"
        ),
    )


def test_superseded_pass_receipts_are_swept_and_the_newest_survives(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _campaign(repo, "alpha")
    old = _receipt(repo, "alpha_old", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    new = _receipt(repo, "alpha_new", "alpha", "PASS", "2026-09-05T00:00:00+00:00")
    _no_citations(monkeypatch)

    plan = mutation_retention.plan(repo)

    assert set(plan.delete) == {old}
    assert plan.keep[new] == "newest PASS for alpha"


def test_the_newest_pass_is_decided_by_the_gates_ordering_not_by_filename(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`(generated_at, name)`, not name alone -- the survivor here sorts first."""

    _campaign(repo, "alpha")
    newest = _receipt(repo, "alpha_a", "alpha", "PASS", "2026-09-05T00:00:00+00:00")
    older = _receipt(repo, "alpha_z", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    _no_citations(monkeypatch)

    plan = mutation_retention.plan(repo)

    assert set(plan.delete) == {older}
    assert newest in plan.keep


def test_a_cited_receipt_survives_even_when_a_newer_pass_exists(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The near-miss this rule exists to prevent.

    The gate cites the newest receipt that ALSO clears `receipt_errors`. When a
    campaign's newest PASS has gone stale against the current runner, the citation
    falls back to an older one, and sweeping on "newest PASS" deletes the receipt
    the gate is actually using -- which cost 11 test files their evidence on the
    real corpus before the citation set became the keep-set's floor.
    """

    _campaign(repo, "alpha")
    cited = _receipt(repo, "alpha_old", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    newer = _receipt(repo, "alpha_new", "alpha", "PASS", "2026-09-05T00:00:00+00:00")
    _cites(monkeypatch, cited)

    plan = mutation_retention.plan(repo)

    assert plan.delete == {}, "a receipt the gate cites must never be swept"
    assert plan.keep[cited] == "read by the coverage gate or the corpus audit"
    assert newer in plan.keep


def test_a_citation_outranks_a_missing_manifest_too(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A vanished manifest is not deletion while the gate still cites the evidence."""

    cited = _receipt(repo, "ghost", "ghost", "PASS", "2026-09-01T00:00:00+00:00")
    _cites(monkeypatch, cited)

    plan = mutation_retention.plan(repo)

    assert plan.delete == {}
    assert cited in plan.keep


def test_receipts_of_a_campaign_with_no_manifest_are_swept(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    orphan = _receipt(repo, "ghost", "ghost", "PASS", "2026-09-01T00:00:00+00:00")
    kept = _receipt(repo, "alpha", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    _campaign(repo, "alpha")
    _no_citations(monkeypatch)

    plan = mutation_retention.plan(repo)

    assert set(plan.delete) == {orphan}
    assert kept in plan.keep


def test_a_manifest_is_matched_by_its_campaign_id_not_its_filename(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The gate keys on `campaign_id`; a manifest may be named anything."""

    _campaign(repo, "alpha", filename="2026-09-07-alpha-rerun")
    kept = _receipt(repo, "alpha", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    _no_citations(monkeypatch)

    plan = mutation_retention.plan(repo)

    assert plan.delete == {}
    assert kept in plan.keep


def test_a_broken_registry_does_not_refuse_the_sweep(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`registry.json` is not a manifest, so it cannot block retention.

    The gate resolves campaigns through manifests and never reads the registry,
    and the two disagree by dozens of campaigns on the real corpus. A registry
    that will not even parse is therefore not retention's problem.
    """

    _campaign(repo, "alpha")
    old = _receipt(repo, "alpha_old", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    _receipt(repo, "alpha_new", "alpha", "PASS", "2026-09-05T00:00:00+00:00")
    registry = repo / mutation_retention.CAMPAIGN_DIRECTORY / "registry.json"
    registry.write_text("{ not json", encoding="utf-8")
    _no_citations(monkeypatch)

    plan = mutation_retention.plan(repo)

    assert set(plan.delete) == {old}


def test_a_missing_campaign_directory_refuses_the_sweep(tmp_path: Path) -> None:
    """Every campaign looks absent from an empty tree -- that is not a sweep."""

    with pytest.raises(RetentionError, match="no campaign directory"):
        mutation_retention.plan(tmp_path)


def test_a_campaign_with_no_pass_keeps_every_receipt(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Broken evidence is not this module's to adjudicate."""

    _campaign(repo, "alpha")
    first = _receipt(repo, "alpha_a", "alpha", "FAIL", "2026-09-01T00:00:00+00:00")
    second = _receipt(repo, "alpha_b", "alpha", "ERROR", "2026-09-05T00:00:00+00:00")
    _no_citations(monkeypatch)

    plan = mutation_retention.plan(repo)

    assert plan.delete == {}
    assert {first, second} <= set(plan.keep)


def test_a_failing_receipt_is_swept_once_a_pass_supersedes_it(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _campaign(repo, "alpha")
    failed = _receipt(repo, "alpha_a", "alpha", "FAIL", "2026-09-01T00:00:00+00:00")
    passed = _receipt(repo, "alpha_b", "alpha", "PASS", "2026-09-05T00:00:00+00:00")
    _no_citations(monkeypatch)

    plan = mutation_retention.plan(repo)

    assert set(plan.delete) == {failed}
    assert passed in plan.keep


def test_an_unparseable_receipt_is_kept_not_swept(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _campaign(repo, "alpha")
    _receipt(repo, "alpha_new", "alpha", "PASS", "2026-09-05T00:00:00+00:00")
    broken = repo / mutation_retention.RECEIPT_DIRECTORY / "broken.json"
    broken.write_text("{ not json", encoding="utf-8")
    _no_citations(monkeypatch)

    plan = mutation_retention.plan(repo)

    assert broken not in plan.delete
    assert broken in plan.keep
    assert plan.unreadable == [broken]


def test_a_receipt_naming_no_campaign_is_kept_not_swept(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Parses, but says nothing retention can act on -- so retention does not act."""

    _campaign(repo, "alpha")
    _receipt(repo, "alpha_new", "alpha", "PASS", "2026-09-05T00:00:00+00:00")
    anonymous = _write_json(
        repo / mutation_retention.RECEIPT_DIRECTORY / "anonymous.json",
        {"status": "PASS", "generated_at": "2026-09-06T00:00:00+00:00"},
    )
    _no_citations(monkeypatch)

    plan = mutation_retention.plan(repo)

    assert anonymous not in plan.delete
    assert plan.unreadable == [anonymous]


def test_a_gate_that_cannot_run_refuses_the_sweep(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """No gate, no sweep: retention never decides evidence is expendable blind."""

    _campaign(repo, "alpha")
    old = _receipt(repo, "alpha_old", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    _receipt(repo, "alpha_new", "alpha", "PASS", "2026-09-05T00:00:00+00:00")

    def explode(repo_root: Path) -> set[Path]:
        raise RetentionError("coverage gate did not run: boom")

    monkeypatch.setattr(mutation_retention, "cited_receipts", explode)

    with pytest.raises(RetentionError, match="coverage gate did not run"):
        mutation_retention.plan(repo)
    assert old.exists()


def test_an_unreadable_manifest_refuses_the_sweep(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A campaign that merely looks absent is the one whose receipts must survive.

    The match is on the manifest's own message, not merely on "unreadable": a
    module that skipped the manifest instead of refusing still reaches a gate it
    cannot run in a temporary tree, and the resulting error reads close enough to
    let the bug through.
    """

    _campaign(repo, "alpha")
    ghost = _receipt(repo, "ghost", "ghost", "PASS", "2026-09-01T00:00:00+00:00")
    (repo / "conductor/mutation_campaigns/alpha.json").write_text("{", encoding="utf-8")
    _no_citations(monkeypatch)

    with pytest.raises(RetentionError, match="manifest alpha.json is unreadable"):
        mutation_retention.plan(repo)
    assert ghost.exists()


def test_a_missing_receipt_directory_refuses_the_sweep(tmp_path: Path) -> None:
    (tmp_path / mutation_retention.CAMPAIGN_DIRECTORY).mkdir(parents=True)

    with pytest.raises(RetentionError, match="no receipt directory"):
        mutation_retention.plan(tmp_path)


def test_protect_overrides_the_rule(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _campaign(repo, "alpha")
    old = _receipt(repo, "alpha_old", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    _receipt(repo, "alpha_new", "alpha", "PASS", "2026-09-05T00:00:00+00:00")
    _no_citations(monkeypatch)

    plan = mutation_retention.plan(repo, protect=[old.name])

    assert plan.delete == {}
    assert plan.keep[old] == "explicitly protected"


def test_plan_touches_nothing_and_apply_removes_exactly_the_planned_files(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _campaign(repo, "alpha")
    old = _receipt(repo, "alpha_old", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    new = _receipt(
        repo, "alpha_new", "alpha", "PASS", "2026-09-05T00:00:00+00:00", pad=512
    )
    _no_citations(monkeypatch)

    plan = mutation_retention.plan(repo)
    assert old.exists(), "plan must not delete anything"
    freed = plan.freed_bytes
    assert freed == old.stat().st_size

    assert mutation_retention.apply(plan) == 1
    assert not old.exists()
    assert new.exists()


def test_cli_reports_without_applying_and_applies_when_told(
    repo: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    _campaign(repo, "alpha")
    old = _receipt(repo, "alpha_old", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    _receipt(repo, "alpha_new", "alpha", "PASS", "2026-09-05T00:00:00+00:00", pad=512)
    _no_citations(monkeypatch)

    assert mutation_retention.main(["--repo-root", str(repo)]) == 0
    report = json.loads(capsys.readouterr().out)
    assert report["deleted"] == 1
    assert report["kept"] == 1
    assert report["freed_bytes"] == old.stat().st_size
    assert "removed" not in report
    assert old.exists()

    assert mutation_retention.main(["--repo-root", str(repo), "--apply"]) == 0
    assert json.loads(capsys.readouterr().out)["removed"] == 1
    assert not old.exists()


def test_cited_receipts_reads_the_receipt_field_of_every_evidence_row(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The citation set comes from `receipt`, not from the test path beside it."""

    from conductor import mutation_coverage

    relative = f"{mutation_retention.RECEIPT_DIRECTORY}/alpha.json"
    monkeypatch.setattr(
        mutation_coverage,
        "coverage_report",
        lambda repo_root: {
            "evidence": [{"path": "conductor/test_alpha.py", "receipt": relative}]
        },
    )

    assert mutation_retention.cited_receipts(repo) == {(repo / relative).resolve()}


def test_a_coverage_gate_that_raises_becomes_a_retention_error(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from conductor import mutation_coverage

    def explode(**kwargs: object) -> dict[str, object]:
        raise RuntimeError("registry is unreadable")

    monkeypatch.setattr(mutation_coverage, "coverage_report", explode)

    with pytest.raises(RetentionError, match="coverage gate did not run"):
        mutation_retention.cited_receipts(repo)


def test_an_evidence_row_without_a_receipt_path_refuses(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A citation the module cannot read is not a citation it may ignore."""

    from conductor import mutation_coverage

    monkeypatch.setattr(
        mutation_coverage,
        "coverage_report",
        lambda repo_root: {"evidence": [{"path": "conductor/test_alpha.py"}]},
    )

    with pytest.raises(RetentionError, match="without a receipt path"):
        mutation_retention.cited_receipts(repo)


def test_a_receipt_the_corpus_audit_reads_survives_a_newer_pass(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The audit is a second authority, not a second opinion.

    It asks a different question of the same directory -- does ANY receipt still
    validate against the current runner -- and answers it per campaign, so it
    vouches for campaigns the coverage inventory never reaches. Sweeping on the
    coverage gate alone cost 33 campaigns their audit evidence.
    """

    _campaign(repo, "alpha")
    old = _receipt(repo, "alpha_old", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    new = _receipt(repo, "alpha_new", "alpha", "PASS", "2026-09-02T00:00:00+00:00")
    monkeypatch.setattr(mutation_retention, "cited_receipts", lambda repo_root: set())
    _audit_accepts(monkeypatch, "alpha_old")

    plan = mutation_retention.plan(repo)

    assert old in plan.keep
    assert old not in plan.delete
    assert new in plan.keep


def test_the_audit_keeps_only_the_receipt_the_audit_itself_would_read(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Newest acceptable per campaign, and among equals the first by filename.

    `mutation_patch_audit` takes `max` over a filename-sorted list, and `max`
    returns the first row holding the highest key. Keeping the last one instead
    would preserve a receipt the audit never opens.
    """

    _campaign(repo, "alpha")
    stale = _receipt(repo, "alpha_a", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    first = _receipt(repo, "alpha_b", "alpha", "PASS", "2026-09-02T00:00:00+00:00")
    second = _receipt(repo, "alpha_c", "alpha", "PASS", "2026-09-02T00:00:00+00:00")
    _audit_accepts(monkeypatch, "alpha_a", "alpha_b", "alpha_c")

    read = mutation_retention.audited_receipts(
        mutation_retention._load_receipts(repo / mutation_retention.RECEIPT_DIRECTORY)[
            0
        ],
        repo,
    )

    assert read == {first.resolve()}
    assert stale.resolve() not in read
    assert second.resolve() not in read


def test_a_receipt_the_audit_rejects_is_never_read(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A receipt that does not validate is not evidence, whatever its status says."""

    _campaign(repo, "alpha")
    _receipt(repo, "alpha_only", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    _audit_accepts(monkeypatch)

    read = mutation_retention.audited_receipts(
        mutation_retention._load_receipts(repo / mutation_retention.RECEIPT_DIRECTORY)[
            0
        ],
        repo,
    )

    assert read == set()


def test_an_audit_that_cannot_read_the_runner_refuses_the_sweep(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Same rule as the coverage gate: no authority, no sweep."""

    from conductor import mutation_patch_audit

    _campaign(repo, "alpha")
    ghost = _receipt(repo, "ghost", "alpha", "PASS", "2026-09-01T00:00:00+00:00")
    monkeypatch.setattr(mutation_retention, "cited_receipts", lambda repo_root: set())

    def _explode() -> dict[str, str]:
        raise RuntimeError("runner components unreadable")

    monkeypatch.setattr(mutation_patch_audit, "_runner_components_sha256", _explode)

    with pytest.raises(RetentionError, match="corpus audit did not run"):
        mutation_retention.plan(repo)
    assert ghost.exists()


def test_compacted_receipts_are_judged_by_their_summary_alone(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The retention sweep is a summary-only reader; slim detail changes nothing.

    After slice L's compaction an old PASS receipt carries a `superseded_by`
    pointer where its per-mutant block was, and the newest keeps a zstd blob.
    The sweep decides on campaign id, status and timestamp -- so the old one is
    still swept and the newest still survives, pointer or not.
    """

    from conductor.mutation_receipt_slim import slim_receipt

    _campaign(repo, "alpha")
    old_payload = {
        "campaign_id": "alpha",
        "status": "PASS",
        "generated_at": "2026-09-01T00:00:00+00:00",
        "mutants": [{"id": f"m{i}", "outcome": "KILLED"} for i in range(80)],
    }
    new_payload = dict(old_payload, generated_at="2026-09-05T00:00:00+00:00")
    old = _write_json(
        repo / mutation_retention.RECEIPT_DIRECTORY / "alpha_old.json",
        {
            **slim_receipt(old_payload),
            "detail": {"encoding": "superseded", "superseded_by": "alpha_new.json"},
        },
    )
    new = _write_json(
        repo / mutation_retention.RECEIPT_DIRECTORY / "alpha_new.json",
        slim_receipt(new_payload),
    )
    _no_citations(monkeypatch)

    plan = mutation_retention.plan(repo)

    assert set(plan.delete) == {old}
    assert plan.keep[new] == "newest PASS for alpha"
