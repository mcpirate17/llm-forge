"""Fixtures for the crate-version check, over a real git repository.

The rule this check exists for is a *comparison against the integration base*, so
every fixture commits a base tree and reads it back through git rather than
handing the check a synthetic OID. A test that stubbed the base read would pass
while the check could not resolve a manifest at all -- which is the only way this
check can fail silently, and the exact shape that let the defect land twice.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.crate_version import (
    _owning_crate,
    check_crate_version,
)
from conductor.candidate_review.model import (
    Candidate,
    Change,
    CheckResult,
    Severity,
    TreeEntry,
)
from conductor.candidate_review.policy import load_policy
from conductor.candidate_review.policy_path import resolve_policy_path

POLICY = load_policy(resolve_policy_path())

MANIFEST = """\
[package]
name = "{name}"
version = "{version}"
edition = "2021"
"""


def _git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args],
        cwd=repo,
        capture_output=True,
        check=True,
        text=True,
    ).stdout.strip()


def _write(root: Path, files: dict[str, str]) -> None:
    for rel, text in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    """A repository whose HEAD tree is the integration base."""

    root = tmp_path / "repo"
    root.mkdir()
    _git(root, "init", "-q")
    _git(root, "config", "user.email", "t@example.invalid")
    _git(root, "config", "user.name", "t")
    return root


def _commit_base(repo: Path, files: dict[str, str]) -> str:
    _write(repo, files)
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", "base")
    return _git(repo, "rev-parse", "HEAD^{tree}")


def _run(
    repo: Path,
    base_tree: str,
    snapshot_files: dict[str, str],
    changed: tuple[str, ...],
) -> CheckResult:
    snapshot = repo.parent / "snapshot"
    _write(snapshot, snapshot_files)
    entries = tuple(
        TreeEntry(path=rel, mode="100644", object_type="blob", oid="0" * 40)
        for rel in sorted(snapshot_files)
    )
    context = ReviewContext(
        repo=repo,
        snapshot=snapshot,
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid=base_tree,
            base_commit_oid=None,
            commit_oid=None,
            target_ref="HEAD",
            changes=tuple(
                Change(
                    status="M",
                    path=rel,
                    old_path=None,
                    old_mode="100644",
                    new_mode="100644",
                    old_oid="0" * 40,
                    new_oid="1" * 40,
                    classes=("native", "source"),
                )
                for rel in changed
            ),
        ),
        entries=entries,
        policy=POLICY,
        surface="manual",
        profile="full",
        owner=None,
        runtime_dir=repo.parent / "runtime",
    )
    return check_crate_version(context)


def _rules(result: CheckResult) -> set[str]:
    return {finding.rule_id for finding in result.findings}


def test_a_bumped_crate_passes(repo: Path) -> None:
    """The false-positive half: doing it right must be silent."""

    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "pub fn old() {}\n",
        },
    )
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.1"
            ),
            "crate/src/lib.rs": "pub fn new() {}\n",
        },
        ("crate/src/lib.rs", "crate/Cargo.toml"),
    )
    assert result.findings == []


def test_the_missing_bump_is_blocking(repo: Path) -> None:
    """The whole reason the check exists: same version, different source.

    Also the severity and the path, asserted rather than assumed -- a finding
    that does not block, or that names the source instead of the manifest the
    author has to edit, does not stop the defect.
    """

    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "pub fn old() {}\n",
        },
    )
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "pub fn new() {}\n",
        },
        ("crate/src/lib.rs",),
    )
    missing = [
        finding
        for finding in result.findings
        if finding.rule_id == "source-changed-without-version-bump"
    ]
    assert missing and all(item.severity is Severity.CRITICAL for item in missing)
    assert missing[0].path == "crate/Cargo.toml"


def test_a_manifest_only_change_needs_no_bump(repo: Path) -> None:
    """Cargo.toml is the file that carries the bump; counting it would demand two."""

    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "pub fn old() {}\n",
        },
    )
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(name="unshipped-crate", version="0.1.0")
            + 'description = "x"\n',
            "crate/src/lib.rs": "pub fn old() {}\n",
        },
        ("crate/Cargo.toml",),
    )
    assert result.findings == []
    assert result.metrics["crates_touched"] == 0


def test_a_new_crate_is_not_asked_to_bump(repo: Path) -> None:
    """There is no base version to differ from, so there is nothing to prove."""

    base = _commit_base(repo, {"README.md": "x\n"})
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "pub fn new() {}\n",
        },
        ("crate/src/lib.rs", "crate/Cargo.toml"),
    )
    assert result.findings == []


def test_a_workspace_member_is_charged_not_its_root(repo: Path) -> None:
    """Deepest manifest wins: a root bump does not license a member's edit."""

    base = _commit_base(
        repo,
        {
            "Cargo.toml": MANIFEST.format(name="unshipped-root", version="0.1.0"),
            "member/Cargo.toml": MANIFEST.format(
                name="unshipped-member", version="0.1.0"
            ),
            "member/src/lib.rs": "pub fn old() {}\n",
        },
    )
    result = _run(
        repo,
        base,
        {
            "Cargo.toml": MANIFEST.format(name="unshipped-root", version="0.2.0"),
            "member/Cargo.toml": MANIFEST.format(
                name="unshipped-member", version="0.1.0"
            ),
            "member/src/lib.rs": "pub fn new() {}\n",
        },
        ("member/src/lib.rs", "Cargo.toml"),
    )
    paths = {finding.path for finding in result.findings}
    assert paths == {"member/Cargo.toml"}


def test_a_non_rust_change_in_a_crate_is_ignored(repo: Path) -> None:
    """Only build inputs change the artifact; a note beside the source does not."""

    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "pub fn old() {}\n",
        },
    )
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "pub fn old() {}\n",
            "crate/NOTES.md": "x\n",
        },
        ("crate/NOTES.md",),
    )
    assert result.findings == []


def test_a_c_source_change_counts_as_a_build_input(repo: Path) -> None:
    """A crate with a build.rs compiling C is still stale-cached by the same key."""

    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/shim.c": "int old(void) { return 0; }\n",
        },
    )
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/shim.c": "int new(void) { return 1; }\n",
        },
        ("crate/src/shim.c",),
    )
    assert "source-changed-without-version-bump" in _rules(result)


def test_an_inherited_workspace_version_is_reported_not_passed(repo: Path) -> None:
    """`version.workspace = true` is not a literal, so no bump can be proven."""

    inherited = '[package]\nname = "unshipped-crate"\nversion.workspace = true\n'
    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": inherited,
            "crate/src/lib.rs": "pub fn old() {}\n",
        },
    )
    result = _run(
        repo,
        base,
        {"crate/Cargo.toml": inherited, "crate/src/lib.rs": "pub fn new() {}\n"},
        ("crate/src/lib.rs",),
    )
    assert _rules(result) == {"unresolvable-crate-version"}


def test_an_unreadable_manifest_is_reported_not_skipped(repo: Path) -> None:
    """A manifest listed in the tree but absent from the snapshot must not pass."""

    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "pub fn old() {}\n",
        },
    )
    snapshot = repo.parent / "snapshot"
    _write(snapshot, {"crate/src/lib.rs": "pub fn new() {}\n"})
    entries = tuple(
        TreeEntry(path=rel, mode="100644", object_type="blob", oid="0" * 40)
        for rel in ("crate/Cargo.toml", "crate/src/lib.rs")
    )
    context = ReviewContext(
        repo=repo,
        snapshot=snapshot,
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid=base,
            base_commit_oid=None,
            commit_oid=None,
            target_ref="HEAD",
            changes=(
                Change(
                    status="M",
                    path="crate/src/lib.rs",
                    old_path=None,
                    old_mode="100644",
                    new_mode="100644",
                    old_oid="0" * 40,
                    new_oid="1" * 40,
                    classes=("native", "source"),
                ),
            ),
        ),
        entries=entries,
        policy=POLICY,
        surface="manual",
        profile="full",
        owner=None,
        runtime_dir=repo.parent / "runtime",
    )
    assert _rules(check_crate_version(context)) == {"unreadable-manifest"}


def test_installed_drift_is_reported_against_the_running_interpreter(
    repo: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A gate run whose wheel is not the tree's build is not evidence for the tree."""

    import conductor.candidate_review.crate_version as module

    monkeypatch.setattr(module, "installed_version", lambda name: "0.9.9")
    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "pub fn old() {}\n",
        },
    )
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.1"
            ),
            "crate/src/lib.rs": "pub fn new() {}\n",
        },
        ("crate/src/lib.rs", "crate/Cargo.toml"),
    )
    drift = [
        finding
        for finding in result.findings
        if finding.rule_id == "installed-version-drift"
    ]
    assert drift and drift[0].severity is Severity.HIGH
    assert "0.9.9" in drift[0].message


def test_an_uninstalled_crate_reports_no_drift(repo: Path) -> None:
    """Most crates ship no wheel; absence must be silence, not a finding."""

    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "pub fn old() {}\n",
        },
    )
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.1"
            ),
            "crate/src/lib.rs": "pub fn new() {}\n",
        },
        ("crate/src/lib.rs", "crate/Cargo.toml"),
    )
    assert "installed-version-drift" not in _rules(result)


def test_the_deepest_crate_owns_a_nested_source() -> None:
    """Unit-level guard on the attribution rule the workspace fixture depends on."""

    crates = ["", "member", "member/inner"]
    assert _owning_crate("member/inner/src/lib.rs", crates) == "member/inner"
    assert _owning_crate("member/src/lib.rs", crates) == "member"
    assert _owning_crate("top.rs", crates) == ""


def test_a_source_outside_every_crate_is_charged_to_nobody() -> None:
    crates = ["member"]
    assert _owning_crate("elsewhere/src/lib.rs", crates) is None


LIB_WITH_TESTS = """\
pub fn shipped() -> u32 {{
    {body}
}}

#[cfg(test)]
mod tests {{
    use super::*;

    #[test]
    fn it_works() {{
        assert_eq!(shipped(), {body});
        let brace = "}}";
        let _ = brace;
    }}
{extra}}}
"""


def test_adding_an_inline_test_does_not_demand_a_bump(repo: Path) -> None:
    """Measured: 4 of 6 candidates this rule fired on were adding `#[cfg(test)]`.

    Those change no shipped byte, and a gate wrong two times out of three is one
    agents route around rather than obey.
    """

    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": LIB_WITH_TESTS.format(body="1", extra=""),
        },
    )
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": LIB_WITH_TESTS.format(
                body="1", extra="\n    #[test]\n    fn also() { assert!(true); }\n"
            ),
        },
        ("crate/src/lib.rs",),
    )
    assert result.findings == []
    assert result.metrics["crates_touched"] == 0


def test_shipped_code_beside_a_test_module_still_demands_a_bump(repo: Path) -> None:
    """The waiver is per-change, not per-file: one real edit re-arms the rule."""

    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": LIB_WITH_TESTS.format(body="1", extra=""),
        },
    )
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": LIB_WITH_TESTS.format(body="2", extra=""),
        },
        ("crate/src/lib.rs",),
    )
    assert "source-changed-without-version-bump" in _rules(result)


def test_an_integration_test_directory_is_not_a_build_input(repo: Path) -> None:
    """Cargo builds `tests/` only under `cargo test`; no wheel byte depends on it."""

    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/tests/it.rs": "#[test]\nfn a() {}\n",
        },
    )
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/tests/it.rs": "#[test]\nfn a() { assert!(true); }\n",
        },
        ("crate/tests/it.rs",),
    )
    assert result.findings == []


def test_an_unparseable_source_demands_the_bump(repo: Path) -> None:
    """Unbalanced braces must fail toward blocking, never toward a silent waiver."""

    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "#[cfg(test)]\nmod t { fn a() {}\n",
        },
    )
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "#[cfg(test)]\nmod t { fn b() {}\n",
        },
        ("crate/src/lib.rs",),
    )
    assert "source-changed-without-version-bump" in _rules(result)


def test_a_new_source_file_is_a_build_input(repo: Path) -> None:
    """Absent from the base means it cannot be compared away as test-only."""

    base = _commit_base(
        repo,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "pub fn a() {}\n",
        },
    )
    result = _run(
        repo,
        base,
        {
            "crate/Cargo.toml": MANIFEST.format(
                name="unshipped-crate", version="0.1.0"
            ),
            "crate/src/lib.rs": "pub fn a() {}\n",
            "crate/src/added.rs": "#[cfg(test)]\nmod t {}\n",
        },
        ("crate/src/added.rs",),
    )
    assert "source-changed-without-version-bump" in _rules(result)


def test_a_raw_string_holding_a_brace_does_not_derail_the_stripper() -> None:
    """A `r#"..."#` literal containing `}` must not close the test module early."""

    from conductor.candidate_review.crate_version import _strip_test_modules

    # The literal has to hold a bare `"` before the `}`: without one, a scanner
    # with no raw-string handling still lands past the brace by accident, and the
    # test passes against a stripper that cannot do this at all.
    text = (
        "pub fn a() {}\n#[cfg(test)]\nmod t {\n"
        '  const S: &str = r#"a " b }"#;\n}\npub fn b() {}\n'
    )
    assert _strip_test_modules(text) == "pub fn a() {}\n\npub fn b() {}\n"


def test_a_commented_brace_does_not_derail_the_stripper() -> None:
    from conductor.candidate_review.crate_version import _strip_test_modules

    text = "pub fn a() {}\n#[cfg(test)]\nmod t {\n  // }\n  /* } */\n}\npub fn b() {}\n"
    assert _strip_test_modules(text) == "pub fn a() {}\n\npub fn b() {}\n"


def test_a_non_block_cfg_test_item_is_stripped_at_its_semicolon() -> None:
    """`#[cfg(test)] use ...;` has no braces; consuming to the next `{` would eat code."""

    from conductor.candidate_review.crate_version import _strip_test_modules

    text = "#[cfg(test)]\nuse std::fmt;\npub fn a() {}\n"
    assert _strip_test_modules(text) == "\npub fn a() {}\n"
