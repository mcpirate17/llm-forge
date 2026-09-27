"""Tests for :mod:`conductor.receipt_verify`.

Fixtures are throwaway git repositories built inside ``tmp_path``: known blobs
are committed so every check has a passing case and a discriminating failing
case with genuinely mismatched values, all measured via ``git cat-file``
against a real tree oid -- never the working tree.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path

from conductor import receipt_verify
from conductor.receipt_verify import (
    EXIT_PASS,
    EXIT_REFUSED,
    PART1,
    PART2,
    PART3,
    PART4,
)

MANIFEST_REL = "conductor/mutation_campaigns/campaign.json"
SOURCES = {
    "src/module.py": b"def add(a, b):\n    return a + b\n",
    "tests/test_module.py": b"def test_add():\n    assert add(1, 2) == 3\n",
}


def _git(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True
    )
    return result.stdout.strip()


def _sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _build_repo(tmp_path: Path) -> tuple[Path, str, bytes, dict[str, str]]:
    """git repo with pinned sources + committed manifest; returns tree state."""
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "--quiet")
    _git(repo, "config", "user.name", "Receipt Verify Test")
    _git(repo, "config", "user.email", "receipt-verify@example.invalid")
    _git(repo, "config", "commit.gpgsign", "false")
    pins = {path: _sha256_hex(data) for path, data in SOURCES.items()}
    manifest_bytes = json.dumps(
        {
            "schema_version": 1,
            "campaign_id": "receipt-verify-fixture",
            "source_sha256": pins,
        },
        indent=1,
    ).encode("utf-8")
    files = dict(SOURCES)
    files[MANIFEST_REL] = manifest_bytes
    for rel, data in files.items():
        target = repo / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    _git(repo, "add", "--", *files)
    _git(repo, "commit", "--quiet", "-m", "fixture tree")
    tree_oid = _git(repo, "rev-parse", "HEAD^{tree}")
    return repo, tree_oid, manifest_bytes, pins


def _write_receipt(
    tmp_path: Path,
    manifest_bytes: bytes,
    pins: Mapping[str, str],
    **overrides: object,
) -> Path:
    """Receipt file OUTSIDE the repo tree: the artifact under authentication."""
    payload: dict[str, object] = {
        "schema_version": "llm.mutation-testing.receipt.v3",
        "campaign_id": "receipt-verify-fixture",
        "status": "PASS",
        "manifest": MANIFEST_REL,
        "manifest_sha256": _sha256_hex(manifest_bytes),
        "source_sha256": dict(pins),
    }
    payload.update(overrides)
    path = tmp_path / "receipt.json"
    path.write_text(json.dumps(payload, indent=1), encoding="utf-8")
    return path


def _run_json(receipt: Path, repo: Path, tree: str, capsys) -> tuple[int, dict]:
    code = receipt_verify.main(
        ["--receipt", str(receipt), "--tree", tree, "--repo", str(repo), "--json"]
    )
    out, _err = capsys.readouterr()
    return code, json.loads(out)


def test_all_four_parts_pass(tmp_path, capsys) -> None:
    repo, tree_oid, manifest_bytes, pins = _build_repo(tmp_path)
    receipt = _write_receipt(tmp_path, manifest_bytes, pins)
    code = receipt_verify.main(
        ["--receipt", str(receipt), "--tree", "HEAD", "--repo", str(repo)]
    )
    out, _err = capsys.readouterr()
    assert code == EXIT_PASS
    lines = out.strip().splitlines()
    # Resolved repo root and tree oid print BEFORE the verdict.
    assert lines[0].startswith("repo_root=")
    assert lines[1] == f"tree_oid={tree_oid}"
    assert lines[2].startswith("PASS ")
    assert f"tree={tree_oid}" in lines[2]
    assert "pins=2" in lines[2]


def test_pass_with_raw_tree_oid(tmp_path, capsys) -> None:
    repo, tree_oid, manifest_bytes, pins = _build_repo(tmp_path)
    receipt = _write_receipt(tmp_path, manifest_bytes, pins)
    code, verdict = _run_json(receipt, repo, tree_oid, capsys)
    assert code == EXIT_PASS
    assert verdict["status"] == "PASS"
    assert verdict["tree_oid"] == tree_oid
    assert {name: check["status"] for name, check in verdict["checks"].items()} == {
        PART1: "PASS",
        PART2: "PASS",
        PART3: "PASS",
        PART4: "PASS",
    }


def test_json_resolution_lines_go_to_stderr(tmp_path, capsys) -> None:
    repo, tree_oid, manifest_bytes, pins = _build_repo(tmp_path)
    receipt = _write_receipt(tmp_path, manifest_bytes, pins)
    code = receipt_verify.main(
        ["--receipt", str(receipt), "--tree", "HEAD", "--repo", str(repo), "--json"]
    )
    out, err = capsys.readouterr()
    assert code == EXIT_PASS
    json.loads(out)  # stdout is exactly one parseable object
    assert f"tree_oid={tree_oid}" in err


def test_refused_on_empty_receipt(tmp_path, capsys) -> None:
    repo, tree_oid, _manifest, _pins = _build_repo(tmp_path)
    stub = tmp_path / "stub-receipt.json"
    stub.write_bytes(b"")  # existence is not content
    code = receipt_verify.main(
        ["--receipt", str(stub), "--tree", tree_oid, "--repo", str(repo)]
    )
    _out, err = capsys.readouterr()
    assert code == EXIT_REFUSED
    # The exact phrase, and a fixture name that cannot contain it: a JSON
    # parse error over a 0-byte file must not satisfy this assertion.
    assert "REFUSED" in err and "receipt file is empty" in err


def test_refused_on_unresolvable_tree(tmp_path, capsys) -> None:
    repo, _tree, manifest_bytes, pins = _build_repo(tmp_path)
    receipt = _write_receipt(tmp_path, manifest_bytes, pins)
    code = receipt_verify.main(
        ["--receipt", str(receipt), "--tree", "deadbeef", "--repo", str(repo)]
    )
    _out, err = capsys.readouterr()
    assert code == EXIT_REFUSED
    assert "REFUSED" in err


def test_module_entrypoint_subprocess(tmp_path) -> None:
    repo, tree_oid, manifest_bytes, pins = _build_repo(tmp_path)
    receipt = _write_receipt(tmp_path, manifest_bytes, pins)
    package_root = Path(__file__).resolve().parents[1]  # locates the package only
    result = subprocess.run(
        [
            sys.executable,
            "-m",
            "conductor.receipt_verify",
            "--receipt",
            str(receipt),
            "--tree",
            "HEAD",
            "--repo",
            str(repo),
        ],
        cwd=package_root,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == EXIT_PASS, result.stderr
    assert f"tree_oid={tree_oid}" in result.stdout
    assert "\nPASS " in result.stdout


def test_repo_defaults_to_cwd_discovery(tmp_path, capsys, monkeypatch) -> None:
    repo, tree_oid, manifest_bytes, pins = _build_repo(tmp_path)
    receipt = _write_receipt(tmp_path, manifest_bytes, pins)
    monkeypatch.chdir(repo / "src")  # discovery must climb to the toplevel
    code = receipt_verify.main(["--receipt", str(receipt), "--tree", "HEAD"])
    out, _err = capsys.readouterr()
    assert code == EXIT_PASS
    assert f"repo_root={repo.resolve()}" in out
    assert f"tree_oid={tree_oid}" in out
