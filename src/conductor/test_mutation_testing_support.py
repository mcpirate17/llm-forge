"""Merging a declared host-read dependency into a snapshot.

Split out of ``test_mutation_testing`` so the merge rules have a module the
campaign generator can pair with the code that implements them.  Every case
here runs through ``mutation_testing._link_host_dependencies``, which is the
seam production uses: it supplies the hard-link ``materialize`` and the
``CampaignError`` type, and delegates the tree walk to
``conductor.mutation_testing_support``.
"""

from __future__ import annotations

import dataclasses
from pathlib import Path

import pytest

from conductor import mutation_testing


def _host_dependency_campaign(
    host: Path, relatives: tuple[str, ...]
) -> mutation_testing.Campaign:
    (host / "reports" / "screen").mkdir(parents=True)
    (host / "reports" / "screen" / "receipt.json").write_text("{}", encoding="utf-8")
    (host / "reports" / "screen" / "ignored.json").write_text("[]", encoding="utf-8")
    return dataclasses.replace(
        mutation_testing.load_campaign(
            mutation_testing.REPO_ROOT
            / "conductor/mutation_campaigns/mutation_framework_self.json"
        ),
        host_read_dependencies=relatives,
    )


def test_host_read_dependency_merge_fills_in_what_is_missing_and_refuses_the_rest(
    tmp_path: Path,
) -> None:
    """The four merge outcomes, in one test because no mutant separates them.

    Twenty-four campaigns declare paths under ``research/reports`` that were
    gitignored scratch when they were authored.  Those directories are only
    *partially* tracked now -- one measured 408 entries on disk against 109
    committed -- so the snapshot holds the committed files and is still missing
    every gitignored one.  Deciding once for the whole directory, either way, is
    wrong: refusing makes the campaign unrunnable, and skipping leaves it reading
    files that are not there.

    The three refusals live here rather than in tests of their own because the
    generated campaign kills every mutant in the merge walk through the fill-in
    case alone: split out, each refusal is dominated and classifies MERGE while
    asserting something the fill-in case does not.  Folded in, the assertions
    survive under one classified nodeid.
    """

    host = tmp_path / "filled" / "host"
    campaign = _host_dependency_campaign(host, ("reports/screen",))
    snapshot = tmp_path / "filled" / "snapshot"
    (snapshot / "reports" / "screen").mkdir(parents=True)
    (snapshot / "reports" / "screen" / "receipt.json").write_text(
        "{}", encoding="utf-8"
    )
    (snapshot / "reports" / "screen" / "tracked_only.json").write_text(
        "0", encoding="utf-8"
    )

    mutation_testing._link_host_dependencies(campaign, snapshot, host)  # noqa: SLF001

    screen = snapshot / "reports" / "screen"
    assert screen.joinpath("ignored.json").read_text(encoding="utf-8") == "[]"
    assert screen.joinpath("receipt.json").read_text(encoding="utf-8") == "{}"
    assert screen.joinpath("tracked_only.json").read_text(encoding="utf-8") == "0"

    # A snapshot copy whose bytes differ from the host is refused, and the
    # refusal names the diverging file rather than the declared directory.
    host = tmp_path / "diverged" / "host"
    campaign = _host_dependency_campaign(host, ("reports/screen",))
    snapshot = tmp_path / "diverged" / "snapshot"
    (snapshot / "reports" / "screen").mkdir(parents=True)
    (snapshot / "reports" / "screen" / "receipt.json").write_text(
        '{"uncommitted": true}', encoding="utf-8"
    )

    with pytest.raises(
        mutation_testing.CampaignError,
        match=r"differs from the host: reports/screen/receipt\.json$",
    ):
        mutation_testing._link_host_dependencies(campaign, snapshot, host)  # noqa: SLF001

    # A directory on the host against a file in the snapshot cannot be merged.
    host = tmp_path / "kind" / "host"
    campaign = _host_dependency_campaign(host, ("reports/screen",))
    snapshot = tmp_path / "kind" / "snapshot"
    (snapshot / "reports").mkdir(parents=True)
    (snapshot / "reports" / "screen").write_text("not a directory", encoding="utf-8")

    with pytest.raises(
        mutation_testing.CampaignError,
        match=r"differs from the host: reports/screen$",
    ):
        mutation_testing._link_host_dependencies(campaign, snapshot, host)  # noqa: SLF001

    # A symlink is never compared: it can point outside the snapshot entirely.
    host = tmp_path / "symlink" / "host"
    campaign = _host_dependency_campaign(host, ("reports/screen",))
    snapshot = tmp_path / "symlink" / "snapshot"
    (snapshot / "reports").mkdir(parents=True)
    (snapshot / "reports" / "screen").symlink_to(host / "reports" / "screen")

    with pytest.raises(
        mutation_testing.CampaignError,
        match=r"already contains host read dependency path: reports/screen$",
    ):
        mutation_testing._link_host_dependencies(campaign, snapshot, host)  # noqa: SLF001
