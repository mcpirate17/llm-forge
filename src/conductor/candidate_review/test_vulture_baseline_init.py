"""Tests for conductor.candidate_review.vulture_baseline_init."""

from __future__ import annotations

import json

import pytest

from conductor.candidate_review import vulture_baseline_init as vbi


class _FakeCompleted:
    def __init__(self, returncode: int, stdout: str = "", stderr: str = ""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


def test_git_tree_chunks_splits_a_real_oid(monkeypatch, tmp_path):
    oid = "abcdef01" * 5
    assert len(oid) == 40

    def fake_run(cmd, **kwargs):
        assert cmd == ["git", "rev-parse", "HEAD"]
        return _FakeCompleted(0, stdout=oid + "\n")

    monkeypatch.setattr(vbi.subprocess, "run", fake_run)
    chunks = vbi.git_tree_chunks(tmp_path)
    assert chunks == ["abcdef01"] * 5
    assert all(len(chunk) == 8 for chunk in chunks)


def test_git_tree_chunks_raises_on_git_failure(monkeypatch, tmp_path):
    def fake_run(cmd, **kwargs):
        return _FakeCompleted(128, stderr="fatal: not a git repository")

    monkeypatch.setattr(vbi.subprocess, "run", fake_run)
    with pytest.raises(vbi.VultureBaselineInitError, match="git rev-parse HEAD failed"):
        vbi.git_tree_chunks(tmp_path)


def test_run_vulture_findings_raises_when_vulture_missing(monkeypatch, tmp_path):
    monkeypatch.setattr(vbi.shutil, "which", lambda name: None)
    with pytest.raises(vbi.VultureBaselineInitError, match="not installed"):
        vbi.run_vulture_findings(tmp_path, ["src"])


def test_run_vulture_findings_raises_on_unexpected_exit_code(monkeypatch, tmp_path):
    monkeypatch.setattr(vbi.shutil, "which", lambda name: "/usr/bin/vulture")
    monkeypatch.setattr(
        vbi.subprocess,
        "run",
        lambda *a, **k: _FakeCompleted(1, stderr="boom"),
    )
    with pytest.raises(vbi.VultureBaselineInitError, match="vulture exited 1"):
        vbi.run_vulture_findings(tmp_path, ["src"])


def test_run_vulture_findings_parses_real_output(monkeypatch, tmp_path):
    monkeypatch.setattr(vbi.shutil, "which", lambda name: "/usr/bin/vulture")
    output = "pkg/mod.py:12: unused variable 'x' (100% confidence)\n"
    monkeypatch.setattr(
        vbi.subprocess, "run", lambda *a, **k: _FakeCompleted(3, stdout=output)
    )
    findings = vbi.run_vulture_findings(tmp_path, ["src"])
    assert len(findings) == 1
    (finding,) = findings.values()
    assert finding["path"] == "pkg/mod.py"
    assert finding["line"] == 12


def test_build_baseline_is_an_empty_allowlist_with_a_real_tree(monkeypatch, tmp_path):
    oid = "0" * 40
    monkeypatch.setattr(
        vbi, "git_tree_chunks", lambda root: [oid[i : i + 8] for i in range(0, 40, 8)]
    )
    monkeypatch.setattr(vbi, "run_vulture_findings", lambda root, paths: {})
    baseline, findings = vbi.build_baseline(tmp_path, ["src"], "2026-09-15")
    assert baseline == {
        "schema_version": 1,
        "generated_from_tree": ["00000000"] * 5,
        "expires": "2026-09-15",
        "count": 0,
        "entries": {},
    }
    assert findings == {}


def test_main_writes_a_schema_valid_baseline_file(monkeypatch, tmp_path):
    monkeypatch.setattr(
        vbi,
        "build_baseline",
        lambda root, paths, expires: (
            {
                "schema_version": 1,
                "generated_from_tree": ["a"] * 5,
                "expires": expires,
                "count": 0,
                "entries": {},
            },
            {},
        ),
    )
    baseline_path = tmp_path / "vulture_baseline.json"
    rc = vbi.main(["--baseline", str(baseline_path), "--expires", "2026-09-15", "src"])
    assert rc == 0
    payload = json.loads(baseline_path.read_text(encoding="utf-8"))
    assert payload["count"] == 0
    assert payload["entries"] == {}
    assert payload["expires"] == "2026-09-15"


def test_main_reports_analyzer_error_without_writing_a_file(
    monkeypatch, tmp_path, capsys
):
    def raise_error(root, paths, expires):
        raise vbi.VultureBaselineInitError("vulture is not installed")

    monkeypatch.setattr(vbi, "build_baseline", raise_error)
    baseline_path = tmp_path / "vulture_baseline.json"
    rc = vbi.main(["--baseline", str(baseline_path), "--expires", "2026-09-15", "src"])
    assert rc == 2
    assert not baseline_path.exists()
    assert "not installed" in capsys.readouterr().err
