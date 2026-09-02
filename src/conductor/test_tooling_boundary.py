"""The tooling boundary holds, and the checker that proves it is not blind.

Half the tests run the checker over this repo (the contract). The other half run it
over synthetic trees under ``tmp_path`` carrying one deliberate coupling each, so a
checker that stops seeing a class of coupling fails here before the contract goes
vacuous. Project package names are assembled from the checker's own constants: this
file must pass the contract it tests, so it never spells one.
"""

from __future__ import annotations

import importlib.util
import textwrap
from pathlib import Path

import pytest

from conductor import _project_hooks, tooling_boundary as tb

REPO_ROOT = Path(__file__).resolve().parents[1]
PACKAGE_DIR = REPO_ROOT / "conductor"
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


def test_rule_a_allowlist_covers_the_default_plugin_string_only(tmp_path: Path) -> None:
    default = _project_hooks.DEFAULT_TEST_PLUGIN
    module = default.split(":")[0]
    pkg = _tree(
        tmp_path,
        {
            "conductor/_project_hooks.py": f"""
                DEFAULT = "{default}"
                OTHER = "{module}_extra:register"
                CHILD = "{module}.child:register"
                def f():
                    from {module} import register
                    return register
                """,
            "conductor/other.py": f'DEFAULT = "{default}"\n',
        },
    )
    assert _lines(tb.check_project_imports(pkg)) == {
        ("conductor/_project_hooks.py", 3),
        ("conductor/_project_hooks.py", 4),
        ("conductor/_project_hooks.py", 6),
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


def test_project_hooks_unset_resolves_the_default_spec(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv(_project_hooks.PLUGIN_ENV, raising=False)
    module_name, function_name = _project_hooks.DEFAULT_TEST_PLUGIN.split(":")
    if importlib.util.find_spec(module_name.split(".")[0]) is None:
        with pytest.raises(ImportError, match=_project_hooks.PLUGIN_ENV):
            _project_hooks.resolve_test_plugin(None)
        return
    plugin = _project_hooks.resolve_test_plugin(None)
    assert plugin is not None and plugin.__name__ == function_name
    calls: list[object] = []
    monkeypatch.setattr(
        _project_hooks, "resolve_test_plugin", lambda spec: calls.append
    )
    _project_hooks.register_test_path_guard("cfg")  # type: ignore[arg-type]
    assert calls == ["cfg"]


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
