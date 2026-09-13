"""`conductor.cost_ledger`: the shim forwards verbatim and never computes.

The `forge` binary is a stub script that records its own argv and exits with
a chosen code, the same boundary `test_cost_budget_audit` stops at: what is
tested here is the glue -- resolve the binary, build the default rollup argv
from the config, forward everything else untouched, propagate the exit code,
and render the report table from a canned audit verdict.
"""

from __future__ import annotations

import json
import stat
from pathlib import Path

import pytest

from conductor import cost_ledger


@pytest.fixture()
def fake_forge(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Path:
    """A `forge` stand-in that records argv and exits `$FORGE_EXIT` (0)."""

    binary = tmp_path / "forge"
    script = tmp_path / "stub-forge.sh"
    script.write_text(
        '#!/bin/sh\n'
        'printf \'%s\\n\' "$@" > "$FORGE_ARGV"\n'
        'exit "${FORGE_EXIT:-0}"\n',
        encoding="utf-8",
    )
    script.chmod(0o755)
    binary.symlink_to(script)
    argv_file = tmp_path / "argv.txt"
    monkeypatch.setenv("FORGE_ARGV", str(argv_file))
    monkeypatch.setattr(cost_ledger, "resolve_forge_binary", lambda root: binary)
    return argv_file


def _recorded_argv(argv_file: Path) -> list[str]:
    return argv_file.read_text(encoding="utf-8").splitlines()


def _config(tmp_path: Path) -> cost_ledger.LedgerConfig:
    transcripts = tmp_path / "-home-x-project"
    transcripts.mkdir()
    return cost_ledger.LedgerConfig(
        ledger_root=tmp_path / "ledger",
        repo_path=tmp_path / "repo",
        project=transcripts.name,
        transcripts_dir=transcripts,
        baseline=tmp_path / "repo" / "ledger" / "cost_budget_baseline.json",
    )


def test_forwarding_subcommands_pass_argv_verbatim_and_propagate_exit(
    fake_forge: Path, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("FORGE_EXIT", "7")
    code = cost_ledger.main(["read", "/a.jsonl", "--json", "--", "--weird"])
    assert code == 7
    argv = _recorded_argv(fake_forge)
    assert argv == [
        "ledger", "read", "/a.jsonl", "--json", "--", "--weird",
    ]


def test_record_forwards_to_audit_record(
    fake_forge: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FORGE_EXIT", "0")
    assert cost_ledger.main(["record", "--window-days", "3"]) == 0
    assert _recorded_argv(fake_forge) == [
        "ledger", "audit", "--record", "--window-days", "3",
    ]


def test_rollup_without_paths_uses_the_project_defaults(
    fake_forge: Path, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    config = _config(tmp_path)
    monkeypatch.setattr(
        cost_ledger,
        "resolve_config",
        lambda **kwargs: config,
    )
    assert cost_ledger.main(["rollup"]) == 0
    assert _recorded_argv(fake_forge) == [
        "ledger",
        "rollup",
        str(config.transcripts_dir),
        "--out",
        str(config.ledger_root),
        "--repo",
        str(config.repo_path),
        "--project",
        config.project,
    ]


def test_rollup_with_paths_forwards_verbatim(
    fake_forge: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Explicit paths are the caller's own rollup; the shim adds nothing."""

    def _must_not_resolve(**kwargs: object) -> None:
        raise AssertionError("resolve_config must not run for explicit paths")

    monkeypatch.setattr(cost_ledger, "resolve_config", _must_not_resolve)
    assert cost_ledger.main(["rollup", "/a.jsonl", "--dry-run"]) == 0
    assert _recorded_argv(fake_forge) == [
        "ledger", "rollup", "/a.jsonl", "--dry-run",
    ]


def test_rollup_defaults_refuse_a_missing_transcript_directory(
    fake_forge: Path, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    config = cost_ledger.LedgerConfig(
        ledger_root=tmp_path / "ledger",
        repo_path=tmp_path / "repo",
        project="-nowhere",
        transcripts_dir=tmp_path / "nowhere",
        baseline=tmp_path / "repo" / "ledger" / "cost_budget_baseline.json",
    )
    monkeypatch.setattr(cost_ledger, "resolve_config", lambda **kwargs: config)
    assert cost_ledger.main(["rollup"]) == 2
    assert not fake_forge.exists(), "forge must never have been spawned"


def test_missing_forge_binary_is_a_loud_exit_2(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(cost_ledger, "resolve_forge_binary", lambda root: None)
    assert cost_ledger.main(["read", "/a.jsonl"]) == 2


def test_report_prints_one_line_per_metric_with_status(
    fake_forge: Path,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    payload = {
        "window": {"from": "2026-09-06", "to": "2026-09-13", "days": 7},
        "metrics": {
            "median_hook_ms": {
                "value": None, "n": 0, "baseline": None,
                "delta_pct": None, "status": "NO_DATA",
            },
            "resend_bytes_per_session": {
                "value": 8815583.0, "n": 20, "baseline": 8815583.0,
                "delta_pct": 0.0, "status": "RATCHET_HELD",
            },
        },
        "status": "NO_DATA",
    }
    script = fake_forge.parent / "stub-forge.sh"
    script.write_text(
        "#!/bin/sh\n"
        '# forge ledger audit refuses to run without --baseline; so must the\n'
        '# stub, or this test cannot see the shim forgetting to pass one.\n'
        'case " $* " in *" --baseline "*) ;; *) echo "--baseline required" >&2;'
        " exit 2 ;; esac\n"
        f"printf '{json.dumps(payload)}\\n'\n",
        encoding="utf-8",
    )
    script.chmod(stat.S_IRWXU)
    config = _config(tmp_path)
    monkeypatch.setattr(cost_ledger, "resolve_config", lambda **kwargs: config)

    code = cost_ledger.main(["report"])

    lines = capsys.readouterr().out.splitlines()
    assert code == 1, "overall NO_DATA is not an OK status"
    assert any(
        "median_hook_ms" in line and "NO_DATA" in line for line in lines
    ), lines
    assert any(
        "resend_bytes_per_session" in line and "RATCHET_HELD" in line
        for line in lines
    ), lines


def test_report_with_unparseable_audit_output_is_a_clean_exit_2(
    fake_forge: Path,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = fake_forge.parent / "stub-forge.sh"
    script.write_text(
        '#!/bin/sh\necho "not json" >&2\nexit 2\n', encoding="utf-8"
    )
    script.chmod(stat.S_IRWXU)
    config = _config(tmp_path)
    monkeypatch.setattr(cost_ledger, "resolve_config", lambda **kwargs: config)
    assert cost_ledger.main(["report"]) == 2
    assert "cost_ledger:" in capsys.readouterr().err


def test_report_propagates_the_hard_empty_window(
    fake_forge: Path,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    script = fake_forge.parent / "stub-forge.sh"
    script.write_text(
        '#!/bin/sh\necho "empty window" >&2\nexit 3\n', encoding="utf-8"
    )
    script.chmod(stat.S_IRWXU)
    config = _config(tmp_path)
    monkeypatch.setattr(cost_ledger, "resolve_config", lambda **kwargs: config)
    assert cost_ledger.main(["report"]) == 3


def test_munged_project_name_matches_the_harness_layout() -> None:
    assert (
        cost_ledger.munged_project_name(Path("/home/tim/Projects/llm-forge"))
        == "-home-tim-Projects-llm-forge"
    )
    assert (
        cost_ledger.transcripts_dir_for(Path("/home/tim/Projects/llm-forge"))
        == cost_ledger.PROJECTS_DIR / "-home-tim-Projects-llm-forge"
    )


def test_resolve_config_prefers_flags_over_env_over_default(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.setenv("LEDGER_ROOT", str(tmp_path / "env-ledger"))
    default = cost_ledger.resolve_config(repo_root=repo)
    assert default.ledger_root == tmp_path / "env-ledger"
    assert default.project == cost_ledger.munged_project_name(repo)

    flagged = cost_ledger.resolve_config(
        repo_root=repo,
        ledger_root=tmp_path / "flag-ledger",
        transcripts_dir=tmp_path / "elsewhere",
    )
    assert flagged.ledger_root == tmp_path / "flag-ledger"
    assert flagged.project == "elsewhere"


def test_has_positional_path_treats_flags_and_separators_correctly() -> None:
    assert cost_ledger._has_positional_path(["/a.jsonl"])
    assert cost_ledger._has_positional_path(["--out", "/x", "/a.jsonl"])
    assert cost_ledger._has_positional_path(["--out=/x", "/a.jsonl"])
    assert cost_ledger._has_positional_path(["--dry-run", "--", "/a.jsonl"])
    # `rollup --dry-run` names no path, so the project defaults still apply.
    assert not cost_ledger._has_positional_path(["--dry-run"])
    assert not cost_ledger._has_positional_path(["--out", "/x", "--dry-run"])
    assert not cost_ledger._has_positional_path([])
