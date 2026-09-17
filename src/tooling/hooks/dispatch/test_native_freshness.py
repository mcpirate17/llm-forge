"""Native freshness: each signal fires on the shape it names, and on nothing else.

Every case builds a whole synthetic checkout -- crate directory, ``pyproject.toml``,
sources, ``.venv/lib/pythonX/site-packages/<dist>.dist-info`` -- because that is what
the module reads. Nothing here imports an extension or touches the real venv.

The four signals each get a pair: the shape that must be reported, and the nearby
shape that must not be. A check that only ever fires is as useless as one that never
does, and a session that is nagged about a venv that is fine learns to skip the block.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from tooling.hooks.dispatch import adapters, registry
from tooling.hooks.dispatch.native_freshness import (
    Crate,
    compiled_extensions,
    crates,
    direct_url,
    dist_info,
    findings,
    report,
    source_digest,
    source_files,
    write_stamp,
)

PYPROJECT = """\
[project]
name = "demo-native"
version = "1.2.3"

[tool.maturin]
module-name = "demo_native"
"""

# A crate that ships no Python extension: `snapshot-retention` and
# `tooling-standalone-smoke` are Rust binaries, and nothing installs them.
NO_MODULE = """\
[project]
name = "demo-binary"
version = "0.2.0"
"""


def make_crate(root: Path, *, version: str = "1.2.3", directory: str = "demo-native"):
    crate = root / "tooling/native" / directory
    (crate / "src").mkdir(parents=True)
    (crate / "pyproject.toml").write_text(PYPROJECT.replace("1.2.3", version))
    (crate / "Cargo.toml").write_text('[package]\nname = "demo-native"\n')
    (crate / "src/lib.rs").write_text("pub fn one() -> u8 { 1 }\n")
    (crate / "README.md").write_text("not a source\n")
    return crate


def install(root: Path, *, version: str = "1.2.3", url: str | None = None) -> Path:
    info = root / f".venv/lib/python3.12/site-packages/demo_native-{version}.dist-info"
    info.mkdir(parents=True)
    (info / "METADATA").write_text("Name: demo-native\n")
    if url is not None:
        (info / "direct_url.json").write_text(json.dumps({"url": url}))
    return info


@pytest.fixture
def checkout(tmp_path: Path) -> Path:
    """A checkout whose venv carries exactly what its crate declares."""
    crate = make_crate(tmp_path)
    install(tmp_path, url=f"file://{crate}")
    write_stamp(tmp_path, crates(tmp_path)[0])
    return tmp_path


def lines(root: Path) -> list[str]:
    return [finding.line() for finding in findings(root)]


# ── what the tree declares ──────────────────────────────────────────────


def test_a_crate_is_read_from_its_pyproject(tmp_path: Path):
    make_crate(tmp_path)
    assert crates(tmp_path) == (
        Crate(
            tmp_path / "tooling/native/demo-native",
            "demo-native",
            "1.2.3",
            "demo_native",
        ),
    )


def test_only_a_crate_declaring_a_module_name_is_installed(tmp_path: Path):
    make_crate(tmp_path)
    binary = tmp_path / "tooling/native/demo-binary"
    binary.mkdir(parents=True)
    (binary / "pyproject.toml").write_text(NO_MODULE)
    bare = tmp_path / "tooling/native/demo-bare"
    bare.mkdir(parents=True)
    (bare / "Cargo.toml").write_text('[package]\nname = "demo-bare"\n')
    assert [crate.distribution for crate in crates(tmp_path)] == ["demo-native"]


def test_a_tree_with_no_native_directory_declares_nothing(tmp_path: Path):
    assert crates(tmp_path) == ()


def test_the_host_names_where_its_crates_are(tmp_path: Path):
    """``tooling/native`` was a module constant until 2026-09-16.

    A host on another layout -- llm-forge keeps its crates in ``native/`` --
    declared no crates as far as this module could see, so every question below
    answered with silence instead of an answer, on the checkout that builds them.
    """
    crate = tmp_path / "crates/demo-native"
    (crate / "src").mkdir(parents=True)
    (crate / "pyproject.toml").write_text(PYPROJECT)
    (tmp_path / "pyproject.toml").write_text(
        '[tool.conductor]\nnative_root = "crates"\n'
    )
    assert [one.distribution for one in crates(tmp_path)] == ["demo-native"]


def test_a_compiled_extension_is_read_from_the_record(tmp_path: Path):
    info = install(tmp_path)
    (info / "RECORD").write_text(
        "demo_native/demo_native.cpython-312-x86_64-linux-gnu.so,,\n"
        "demo_native/__init__.py,,\n"
    )
    assert compiled_extensions(info) == (
        "demo_native/demo_native.cpython-312-x86_64-linux-gnu.so",
    )


def test_a_distribution_from_an_index_records_no_origin(tmp_path: Path):
    """PEP 610 writes ``direct_url.json`` only for a path, archive or VCS install."""
    assert direct_url(install(tmp_path)) == {}
    assert direct_url(install(tmp_path, version="9.9.9", url="file:///x"))["url"] == (
        "file:///x"
    )


def test_the_make_target_names_the_directory_not_the_distribution(tmp_path: Path):
    make_crate(tmp_path, directory="demo-native-crate")
    crate = crates(tmp_path)[0]
    assert crate.distribution == "demo-native"
    assert crate.make_target == "demo-native-crate"


# ── what the venv carries ───────────────────────────────────────────────


def test_a_dist_info_is_matched_by_escaped_name_not_by_prefix(tmp_path: Path):
    packages = tmp_path / ".venv/lib/python3.12/site-packages"
    (packages / "demo_native_extras-9.9.dist-info").mkdir(parents=True)
    assert dist_info(packages, "demo-native") is None
    (packages / "demo_native-1.2.3.dist-info").mkdir()
    found = dist_info(packages, "demo-native")
    assert found is not None and found.name == "demo_native-1.2.3.dist-info"


# ── the four signals, each with the shape it must not fire on ───────────


def test_a_crate_the_venv_never_installed_is_reported(tmp_path: Path):
    make_crate(tmp_path)
    (tmp_path / ".venv/lib/python3.12/site-packages").mkdir(parents=True)
    assert lines(tmp_path) == [
        "demo-native: not installed in .venv; `make demo-native` builds it"
    ]


def test_an_installed_version_behind_the_crate_is_reported(tmp_path: Path):
    crate = make_crate(tmp_path)
    install(tmp_path, version="1.2.2", url=f"file://{crate}")
    write_stamp(tmp_path, crates(tmp_path)[0])
    assert lines(tmp_path) == [
        "demo-native: installed 1.2.2, this tree declares 1.2.3; `make demo-native`"
    ]


def test_a_wheel_built_in_another_checkout_is_reported(tmp_path: Path):
    make_crate(tmp_path)
    install(tmp_path, url="file:///elsewhere/tooling/native/demo-native")
    write_stamp(tmp_path, crates(tmp_path)[0])
    assert lines(tmp_path) == [
        "demo-native: built from /elsewhere/tooling/native/demo-native, "
        "not this checkout; `make demo-native`"
    ]


def test_an_installer_that_recorded_no_source_is_not_accused(tmp_path: Path):
    make_crate(tmp_path)
    install(tmp_path)
    write_stamp(tmp_path, crates(tmp_path)[0])
    assert lines(tmp_path) == []


def test_sources_changed_since_the_build_are_reported(checkout: Path):
    (checkout / "tooling/native/demo-native/src/lib.rs").write_text(
        "pub fn one() -> u8 { 2 }\n"
    )
    assert lines(checkout) == [
        "demo-native: crate sources changed since the last build; `make demo-native`"
    ]


def test_a_build_that_left_no_stamp_is_not_accused_of_drift(tmp_path: Path):
    crate = make_crate(tmp_path)
    install(tmp_path, url=f"file://{crate}")
    (crate / "src/lib.rs").write_text("pub fn one() -> u8 { 2 }\n")
    assert lines(tmp_path) == []


def test_a_checkout_with_no_venv_has_nothing_to_compare(tmp_path: Path):
    make_crate(tmp_path)
    assert findings(tmp_path) == ()


def test_a_venv_that_matches_the_tree_says_nothing(checkout: Path):
    assert findings(checkout) == ()
    assert report(checkout) == ""


# ── the digest: what a rebuild would actually read ──────────────────────


def test_cargos_own_build_output_is_not_a_source_change(checkout: Path):
    generated = checkout / "tooling/native/demo-native/target/release/build/dep/out"
    generated.mkdir(parents=True)
    (generated / "generated_alias.rs").write_text("// regenerated every build\n")
    assert lines(checkout) == []


def test_the_digest_covers_the_manifests_as_well_as_the_rust(checkout: Path):
    crate = checkout / "tooling/native/demo-native"
    before = source_digest(crate)
    (crate / "Cargo.toml").write_text(
        '[package]\nname = "demo-native"\nedition = "2021"\n'
    )
    assert source_digest(crate) != before


def test_a_renamed_source_changes_the_digest(checkout: Path):
    crate = checkout / "tooling/native/demo-native"
    before = source_digest(crate)
    (crate / "src/lib.rs").rename(crate / "src/main.rs")
    assert source_digest(crate) != before


def test_the_digest_reads_only_rust_and_manifests(checkout: Path):
    crate = checkout / "tooling/native/demo-native"
    assert {path.name for path in source_files(crate)} == {
        "pyproject.toml",
        "Cargo.toml",
        "lib.rs",
    }


# ── the report and the hook that prints it ──────────────────────────────


def test_the_report_names_the_checkout_it_judged(checkout: Path):
    (checkout / "tooling/native/demo-native/src/lib.rs").write_text("// changed\n")
    assert report(checkout).startswith(f"NATIVE TOOLING out of date in {checkout}:")


class _Ctx:
    def __init__(self, payload: dict) -> None:
        self.root = Path("/nonexistent")
        self.event = "SessionStart"
        self.payload = payload


def _stub_gate(monkeypatch: pytest.MonkeyPatch, checkout: Path | None) -> None:
    """Stand in for ``crg_gate``: the adapter's only outside dependency."""

    class Gate:
        @staticmethod
        def session_checkout(payload: dict) -> Path:
            if checkout is None:
                raise RuntimeError("no checkout")
            return checkout

    monkeypatch.setattr(adapters, "_body", lambda ctx, relative: Gate)


def test_the_adapter_injects_the_report_as_session_context(
    checkout: Path, monkeypatch: pytest.MonkeyPatch
):
    (checkout / "tooling/native/demo-native/src/lib.rs").write_text("// changed\n")
    _stub_gate(monkeypatch, checkout)
    output = adapters.native_freshness_report(_Ctx({}))
    assert output is not None
    specific = output["hookSpecificOutput"]
    assert specific["hookEventName"] == "SessionStart"
    assert "crate sources changed since the last build" in specific["additionalContext"]


def test_the_adapter_never_raises_into_a_session(monkeypatch: pytest.MonkeyPatch):
    _stub_gate(monkeypatch, None)
    output = adapters.native_freshness_report(_Ctx({}))
    assert output is not None
    assert "no checkout" in output["systemMessage"]
    assert "hookSpecificOutput" not in output


def test_the_hook_is_registered_on_session_start():
    spec = next(hook for hook in registry.HOOKS if hook.name == "native_freshness")
    assert spec.event == "SessionStart"
    assert spec.adapter == "native_freshness_report"
    assert not spec.fail_closed
    assert spec.timeout <= registry.event_timeout("SessionStart")


_SERVER_BLOCK = "GRAPH SERVER natives out of date in /x:\n- demo-native: 1 installed, 2 declared; `make crg-sync`"


def _stub_server_report(monkeypatch: pytest.MonkeyPatch, text: str) -> None:
    """The graph server's half, whose own contracts live in test_crg_venv_sync."""
    monkeypatch.setattr(adapters.crg_venv_sync, "session_report", lambda root: text)


def test_the_adapter_carries_both_interpreters(
    checkout: Path, monkeypatch: pytest.MonkeyPatch
):
    (checkout / "tooling/native/demo-native/src/lib.rs").write_text("// changed\n")
    _stub_gate(monkeypatch, checkout)
    _stub_server_report(monkeypatch, _SERVER_BLOCK)
    context = adapters.native_freshness_report(_Ctx({}))["hookSpecificOutput"][
        "additionalContext"
    ]
    assert "NATIVE TOOLING out of date" in context
    assert "GRAPH SERVER natives out of date" in context
    assert "\n\n" in context  # two blocks, not one run-on paragraph


def test_a_stale_graph_server_alone_is_worth_a_block(
    checkout: Path, monkeypatch: pytest.MonkeyPatch
):
    """The venv can be in step while the interpreter that runs the server is not."""
    _stub_gate(monkeypatch, checkout)
    _stub_server_report(monkeypatch, _SERVER_BLOCK)
    output = adapters.native_freshness_report(_Ctx({}))
    assert output is not None
    assert output["hookSpecificOutput"]["additionalContext"] == _SERVER_BLOCK
