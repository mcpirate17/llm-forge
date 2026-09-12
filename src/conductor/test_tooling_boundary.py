"""The tooling boundary holds, and the checker that proves it is not blind.

Half the tests run the checker over this repo (the contract). The other half run it
over synthetic trees under ``tmp_path`` carrying one deliberate coupling each, so a
checker that stops seeing a class of coupling fails here before the contract goes
vacuous. Project package names are assembled from the checker's own constants: this
file must pass the contract it tests, so it never spells one.
"""

from __future__ import annotations

import re
import sys
import textwrap
from types import ModuleType
from pathlib import Path

import pytest

from conductor import _project_hooks, tooling_boundary as tb
from conductor.project_paths import package_path, package_tree_root

PACKAGE_DIR = Path(__file__).resolve().parents[0]
# The tree root, per its own configuration -- not a fixed parents[n] walk, which
# under this repository's src layout names ``src/`` and hides the CLI's own
# resolution behind an accident that happens to work.
REPO_ROOT = package_tree_root(PACKAGE_DIR)
PKG = tb.PROJECT_PACKAGES[0]
NATIVE = tb.NATIVE_CRATE


def _tree(tmp_path: Path, files: dict[str, str]) -> Path:
    """Write ``files`` (root-relative) and return the conductor package dir."""
    for rel, body in files.items():
        path = tmp_path / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(body), encoding="utf-8")
    return tmp_path / "conductor"


def _seam_source() -> str:
    return (PACKAGE_DIR / tb.NATIVE_SEAM).read_text(encoding="utf-8")


def _lines(violations: list[tb.Violation]) -> set[tuple[str, int]]:
    return {(v.path, v.line) for v in violations}


# --- the contract over this repo ---------------------------------------------


def test_rule_a_no_conductor_module_reaches_a_project_package() -> None:
    assert [str(v) for v in tb.check_project_imports(PACKAGE_DIR)] == []


def test_rule_b_generic_hooks_carry_no_project_literal() -> None:
    dirs = tb.default_hook_dirs(PACKAGE_DIR)
    assert dirs, "no hook tree found next to the package"
    assert [str(v) for v in tb.check_hook_literals(dirs, PACKAGE_DIR)] == []


def test_rule_c_non_test_modules_carry_no_host_path_literal() -> None:
    assert [str(v) for v in tb.check_path_literals(PACKAGE_DIR)] == []


def test_rule_d_native_seam_is_the_only_seam_and_exports_real_symbols() -> None:
    assert [str(v) for v in tb.check_native_seam(PACKAGE_DIR)] == []


def test_cli_reports_clean_on_this_repo(capsys: pytest.CaptureFixture[str]) -> None:
    assert tb.main(["--root", str(REPO_ROOT)]) == 0
    out = capsys.readouterr().out
    assert "findings=0" in out and "rule d: clean" in out


def test_repo_root_is_the_tree_that_declares_the_package() -> None:
    """The CLI is handed a tree root, and resolves the package from that tree."""
    assert package_path(REPO_ROOT).resolve() == PACKAGE_DIR
    assert (REPO_ROOT / "pyproject.toml").is_file()


def test_cli_refuses_a_root_with_no_package_naming_the_configured_path(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    (tmp_path / "pyproject.toml").write_text(
        '[tool.conductor]\npackage_root = "src/conductor"\n', encoding="utf-8"
    )
    assert tb.main(["--root", str(tmp_path)]) == 2
    assert "no src/conductor/ under" in capsys.readouterr().err


def test_cli_finds_a_package_the_tree_declares_under_src(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A src-layout tree is checked, not refused as package-less."""
    (tmp_path / "pyproject.toml").write_text(
        '[tool.conductor]\npackage_root = "src/conductor"\n', encoding="utf-8"
    )
    package = tmp_path / "src" / "conductor"
    package.mkdir(parents=True)
    seen: list[Path] = []
    monkeypatch.setattr(
        tb, "check_all", lambda directory: seen.append(directory) or {"a": []}
    )
    assert tb.main(["--root", str(tmp_path)]) == 0
    assert seen == [package]


def test_allowlist_entries_carry_a_reason_and_name_existing_files() -> None:
    for rel, kind, module, reason in tb.ALLOWLIST:
        assert (PACKAGE_DIR / rel).is_file(), rel
        assert kind in {"import", "string", "literal"}
        assert module and reason


# --- rule a: the checker sees every scope ------------------------------------


def test_rule_a_flags_module_scope_import_with_file_and_line(tmp_path: Path) -> None:
    pkg = _tree(tmp_path, {"conductor/x.py": f"import os\nimport {PKG}.tools\n"})
    found = tb.check_project_imports(pkg)
    assert [str(v) for v in found] == [
        f"conductor/x.py:2: [a] imports project module {PKG}.tools"
    ]


def test_rule_a_flags_function_scope_and_try_except_imports(tmp_path: Path) -> None:
    pkg = _tree(
        tmp_path,
        {
            "conductor/x.py": f"""
                def f():
                    from {PKG}.tools.thing import X
                    return X
                try:
                    import {tb.PROJECT_PACKAGES[1]}
                except ImportError:
                    pass
                """
        },
    )
    assert _lines(tb.check_project_imports(pkg)) == {
        ("conductor/x.py", 3),
        ("conductor/x.py", 6),
    }


def test_rule_a_flags_importlib_string_literal(tmp_path: Path) -> None:
    pkg = _tree(
        tmp_path,
        {
            "conductor/x.py": f"""
                import importlib
                def load():
                    return importlib.import_module("{PKG}.tools.thing")
                """
        },
    )
    found = tb.check_project_imports(pkg)
    assert len(found) == 1 and found[0].line == 4
    assert f"{PKG}.tools.thing" in found[0].message


def test_rule_a_ignores_non_module_strings(tmp_path: Path) -> None:
    pkg = _tree(
        tmp_path,
        {
            "conductor/x.py": f'''
                A = "{PKG}.tools.thing train"
                B = "{PKG}/notes/kb.md"
                C = "{PKG}"
                D = "{PKG}.x"
                E = "{PKG}:fn"
                '''
        },
    )
    found = tb.check_project_imports(pkg)
    assert [v.line for v in found] == [5, 6]


def test_rule_a_rejects_host_plugin_string_in_generic_module(tmp_path: Path) -> None:
    plugin = f"{PKG}.tests._path_guard:register"
    pkg = _tree(
        tmp_path,
        {
            "conductor/_project_hooks.py": f'PLUGIN = "{plugin}"\n',
            "conductor/other.py": f'PLUGIN = "{plugin}"\n',
        },
    )
    assert _lines(tb.check_project_imports(pkg)) == {
        ("conductor/_project_hooks.py", 1),
        ("conductor/other.py", 1),
    }


def test_rule_a_scans_tests_too(tmp_path: Path) -> None:
    pkg = _tree(tmp_path, {"conductor/tests/test_x.py": f"import {PKG}\n"})
    assert _lines(tb.check_project_imports(pkg)) == {("conductor/tests/test_x.py", 1)}


# --- rule b ----------------------------------------------------------------------


# spelled out, never taken from the checker: a dropped entry must fail here
@pytest.mark.parametrize("literal", ("research/", "/home/tim", "/mnt/data"))
def test_rule_b_flags_each_literal_in_generic_hooks(
    tmp_path: Path, literal: str
) -> None:
    pkg = _tree(
        tmp_path,
        {
            "conductor/x.py": "",
            ".claude/hooks/pre.sh": f"#!/bin/sh\necho ok\nls {literal}x\n",
            ".claude/hooks/project/env.sh": f"export X={literal}\n",
            ".claude/hooks/test_pre.py": f"X = '{literal}'\n",
            ".claude/hooks/__pycache__/pre.cpython-312.pyc": f"{literal}/pre.py\n",
            ".agent_hooks/guard.py": f"P = '{literal}'\n",
        },
    )
    found = tb.check_hook_literals(tb.default_hook_dirs(pkg), pkg)
    assert _lines(found) == {(".claude/hooks/pre.sh", 3), (".agent_hooks/guard.py", 1)}
    assert all(repr(literal) in v.message for v in found)


def test_rule_b_default_hook_dirs_cover_repo_and_standalone_layouts(
    tmp_path: Path,
) -> None:
    pkg = _tree(tmp_path, {"src/conductor/x.py": "", "hooks/pre.sh": "ls /mnt/data\n"})
    dirs = tb.default_hook_dirs(pkg.parent / "src" / "conductor")
    assert dirs == [tmp_path / "hooks"]
    moved = _tree(
        tmp_path / "moved" / "repo",
        {"conductor/x.py": "", "tooling/hooks/claude/pre.sh": "ls /mnt/data\n"},
    )
    assert tb.default_hook_dirs(moved) == [moved.parent / "tooling" / "hooks"]
    nowhere = tmp_path / "a" / "b" / "conductor"
    assert tb.default_hook_dirs(nowhere) == []
    with pytest.raises(FileNotFoundError):
        tb.check_all(nowhere)


# --- rule c ----------------------------------------------------------------------


@pytest.mark.parametrize("literal", ("/home/tim", "/mnt/data"))
def test_rule_c_flags_host_paths_in_non_test_modules_only(
    tmp_path: Path, literal: str
) -> None:
    pkg = _tree(
        tmp_path,
        {
            "conductor/x.py": f"# {literal} in a comment counts\nP = '{literal}/x'\n",
            "conductor/test_x.py": f"P = '{literal}'\n",
            "conductor/tests/test_y.py": f"P = '{literal}'\n",
        },
    )
    assert _lines(tb.check_path_literals(pkg)) == {
        ("conductor/x.py", 1),
        ("conductor/x.py", 2),
    }


# --- rule d ----------------------------------------------------------------------


def test_rule_d_flags_a_symbol_the_crate_does_not_export(tmp_path: Path) -> None:
    pkg = _tree(
        tmp_path,
        {
            "conductor/_native.py": (
                f"from {NATIVE} import (\n"
                "    validate_mutation_receipt_native,\n"
                "    definitely_not_a_symbol_native,\n"
                ")\n"
            )
        },
    )
    found = tb.check_native_seam(pkg)
    assert [str(v) for v in found] == [
        f"conductor/_native.py:1: [d] definitely_not_a_symbol_native is not exported by {NATIVE}"
    ]


def test_rule_d_flags_the_crate_named_outside_the_seam(tmp_path: Path) -> None:
    pkg = _tree(
        tmp_path,
        {
            "conductor/_native.py": _seam_source(),
            "conductor/x.py": f"""
                def f():
                    import {NATIVE}
                    return {NATIVE}
                """,
            "conductor/y.py": f'import importlib\nm = importlib.import_module("{NATIVE}")\n',
        },
    )
    assert _lines(tb.check_native_seam(pkg)) == {
        ("conductor/x.py", 3),
        ("conductor/y.py", 2),
    }


def test_rule_d_flags_a_seam_importing_anything_but_the_crate(tmp_path: Path) -> None:
    pkg = _tree(
        tmp_path,
        {
            "conductor/_native.py": f"import {PKG}\nfrom {NATIVE} import validate_mutation_receipt_native\n"
        },
    )
    found = tb.check_native_seam(pkg)
    assert [str(v) for v in found] == [
        f"conductor/_native.py:1: [d] seam imports {PKG}, not the crate"
    ]


# --- _project_hooks: the guard is configuration -----------------------------------


def _plugin_config(tmp_path: Path, content: str) -> object:
    (tmp_path / "pyproject.toml").write_text(content, encoding="utf-8")
    return type("Config", (), {"rootpath": tmp_path})()


def test_project_hooks_unset_without_configuration_resolves_none(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.delenv(_project_hooks.PLUGIN_ENV, raising=False)
    assert _project_hooks.resolve_test_plugin(None) is None
    calls: list[tuple[str | None, str]] = []
    monkeypatch.setattr(
        _project_hooks,
        "resolve_test_plugin",
        lambda spec, *, source: calls.append((spec, source)),
    )
    config = type("Config", (), {"rootpath": tmp_path})()
    _project_hooks.register_test_path_guard(config)  # type: ignore[arg-type]
    assert calls == [(None, _project_hooks._CONFIG_KEY)]  # noqa: SLF001


def test_project_hooks_invokes_configured_callable(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    config = _plugin_config(
        tmp_path,
        '[tool.conductor.pytest]\ntest_plugin = "test_host_plugin:register"\n',
    )
    host = ModuleType("test_host_plugin")
    received: list[object] = []
    host.register = received.append  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, host.__name__, host)
    monkeypatch.delenv(_project_hooks.PLUGIN_ENV, raising=False)
    _project_hooks.register_test_path_guard(config)  # type: ignore[arg-type]
    assert received == [config]


def test_project_hooks_environment_bypasses_malformed_config(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    config = _plugin_config(tmp_path, "[tool")
    monkeypatch.setenv(_project_hooks.PLUGIN_ENV, "")
    _project_hooks.register_test_path_guard(config)  # type: ignore[arg-type]
    calls: list[tuple[str | None, str]] = []
    monkeypatch.setattr(
        _project_hooks,
        "resolve_test_plugin",
        lambda spec, *, source: calls.append((spec, source)),
    )
    monkeypatch.setenv(_project_hooks.PLUGIN_ENV, "override.plugin:register")
    _project_hooks.register_test_path_guard(config)  # type: ignore[arg-type]
    assert calls == [("override.plugin:register", _project_hooks.PLUGIN_ENV)]


def test_project_hooks_refuse_unreadable_or_oversized_config(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    config = _plugin_config(tmp_path, "[tool]\n")
    config_path = tmp_path / "pyproject.toml"
    original_open = Path.open

    def denied(self: Path, *args: object, **kwargs: object) -> object:
        if self == config_path:
            raise PermissionError("denied")
        return original_open(self, *args, **kwargs)

    monkeypatch.setattr(Path, "open", denied)
    with pytest.raises(ValueError, match="cannot read configuration"):
        _project_hooks._configured_test_plugin(config)  # noqa: SLF001
    monkeypatch.undo()
    config = _plugin_config(tmp_path, "x" * (_project_hooks._CONFIG_LIMIT + 1))  # noqa: SLF001
    with pytest.raises(ValueError, match="exceeds 64 KiB"):
        _project_hooks._configured_test_plugin(config)  # noqa: SLF001


@pytest.mark.parametrize(
    "content",
    ("", "[tool]\n", "[tool.conductor]\n", "[tool.conductor.pytest]\n"),
)
def test_project_hooks_missing_config_sections_resolve_none(
    tmp_path: Path, content: str
) -> None:
    selector, _source = _project_hooks._configured_test_plugin(  # noqa: SLF001
        _plugin_config(tmp_path, content)  # type: ignore[arg-type]
    )
    assert selector is None


def test_project_hooks_config_and_environment_precedence(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    config = _plugin_config(
        tmp_path,
        '[tool.conductor.pytest]\ntest_plugin = "host.plugin:register"\n',
    )
    calls: list[tuple[str | None, str]] = []
    monkeypatch.setattr(
        _project_hooks,
        "resolve_test_plugin",
        lambda spec, *, source: calls.append((spec, source)),
    )
    monkeypatch.delenv(_project_hooks.PLUGIN_ENV, raising=False)
    _project_hooks.register_test_path_guard(config)  # type: ignore[arg-type]
    monkeypatch.setenv(_project_hooks.PLUGIN_ENV, "")
    _project_hooks.register_test_path_guard(config)  # type: ignore[arg-type]
    monkeypatch.setenv(_project_hooks.PLUGIN_ENV, "override.plugin:register")
    _project_hooks.register_test_path_guard(config)  # type: ignore[arg-type]
    assert calls[0][0] == "host.plugin:register"
    assert calls[0][1].endswith(_project_hooks._CONFIG_KEY)  # noqa: SLF001
    assert calls[1:] == [
        ("", _project_hooks.PLUGIN_ENV),
        ("override.plugin:register", _project_hooks.PLUGIN_ENV),
    ]


@pytest.mark.parametrize(
    ("content", "match"),
    (
        ("[tool", "invalid TOML configuration"),
        ("[tool]\nconductor = []\n", "[tool.conductor] must be a table"),
        ("[tool.conductor]\npytest = []\n", "[tool.conductor.pytest] must be a table"),
        (
            "[tool.conductor.pytest]\ntest_plugin = 1\n",
            "test_plugin must be a string",
        ),
    ),
)
def test_project_hooks_refuse_malformed_or_wrong_type_config(
    tmp_path: Path, content: str, match: str
) -> None:
    with pytest.raises(ValueError, match=re.escape(match)):
        _project_hooks._configured_test_plugin(  # noqa: SLF001
            _plugin_config(tmp_path, content)  # type: ignore[arg-type]
        )


def test_project_hooks_refuse_noncallable_configured_attribute(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.delenv(_project_hooks.PLUGIN_ENV, raising=False)
    config = _plugin_config(
        tmp_path,
        '[tool.conductor.pytest]\ntest_plugin = "conductor._project_hooks:PLUGIN_ENV"\n',
    )
    with pytest.raises(TypeError, match="not callable"):
        _project_hooks.register_test_path_guard(config)  # type: ignore[arg-type]


def test_project_hooks_empty_spec_means_no_guard(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    assert _project_hooks.resolve_test_plugin("") is None
    monkeypatch.setenv(_project_hooks.PLUGIN_ENV, "")

    class Config:
        def __getattr__(self, name: str) -> None:
            raise AssertionError(f"config touched: {name}")

    _project_hooks.register_test_path_guard(Config())  # type: ignore[arg-type]


def test_project_hooks_bogus_spec_fails_loud(monkeypatch: pytest.MonkeyPatch) -> None:
    for spec in ("no_such_module_xyz:register", "conductor.atomic_json:no_such_fn"):
        monkeypatch.setenv(_project_hooks.PLUGIN_ENV, spec)
        with pytest.raises(ImportError, match=_project_hooks.PLUGIN_ENV):
            _project_hooks.register_test_path_guard(None)  # type: ignore[arg-type]
    with pytest.raises(ValueError, match="module:function"):
        _project_hooks.resolve_test_plugin("conductor.atomic_json")
