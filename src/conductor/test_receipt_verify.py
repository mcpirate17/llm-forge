# Mutation-campaign design for conductor/receipt_verify.py (DESIGN ONLY --
# deliberately NOT registered; conductor/mutation_campaigns/registry.json is
# untouched). One first-order mutant per verdict check, each turning that check
# vacuous, and the single test that kills it. Every killer asserts the SPECIFIC
# part's verdict from the --json object, not merely the exit code, so a mutant
# that reroutes the failure to a different part still dies.
#
#   RV-M1 (part 1 vacuous): in _tree_blob(), invert the returncode guard
#         (`result.returncode != 0` -> `result.returncode != result.returncode`)
#         so a missing manifest path yields b"" and part 1 "passes".
#         Killed by: test_part1_missing_manifest_path_fails -- asserts part 1
#         FAIL and parts 2-4 BLOCKED; under the mutant part 1 reads PASS.
#   RV-M2 (part 2 vacuous): in verify_receipt(), flip the manifest-hash
#         comparison (`actual_manifest_sha == receipt.manifest_sha256` ->
#         `actual_manifest_sha == actual_manifest_sha`, always true).
#         Killed by: test_part2_manifest_sha256_mismatch_fails -- the recorded
#         hash is a genuine sha256 of different bytes; part 2 must FAIL and
#         name both hashes.
#   RV-M3 (part 3 vacuous): neutralize the pin comparison
#         (`actual != pinned` -> `actual[:0] != pinned[:0]`, always false --
#         the truncated-hash-input form of the same vacuity).
#         Killed by: test_part3_drifted_source_pin_fails -- a genuinely drifted
#         blob must produce a part 3 FAIL naming the path and both hashes.
#   RV-M4 (part 4 vacuous, comparison): flip the digest comparison
#         (`recorded_digest == reproduced_digest` -> always true).
#         Killed by: test_part4_receipt_map_divergence_fails_alone -- parts 1-3
#         PASS while part 4 must FAIL with differing digests.
#   RV-M5 (part 4 vacuous, sort dropped): delete `lines.sort(...)` in
#         inventory_digest(), leaving dict insertion order. Invisible to the
#         end-to-end CLI runs (both sides use the same function), which is
#         exactly why the killer builds its expectation independently.
#         Killed by: test_inventory_digest_matches_independent_reference --
#         pins whose insertion order differs from hash order, reference digest
#         hand-built in sha256sum format with hash-sorted lines.
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
    EXIT_FAIL,
    EXIT_PASS,
    EXIT_REFUSED,
    PART1,
    PART2,
    PART3,
    PART4,
    inventory_digest,
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


def _commit_change(repo: Path, rel: str, data: bytes | None) -> str:
    """Overwrite (or delete) one tracked file, commit, return the new tree oid."""
    if data is None:
        _git(repo, "rm", "--quiet", "--", rel)
    else:
        (repo / rel).write_bytes(data)
        _git(repo, "add", "--", rel)
    _git(repo, "commit", "--quiet", "-m", f"change {rel}")
    return _git(repo, "rev-parse", "HEAD^{tree}")


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


def test_part1_missing_manifest_path_fails(tmp_path, capsys) -> None:
    repo, tree_oid, manifest_bytes, pins = _build_repo(tmp_path)
    receipt = _write_receipt(
        tmp_path,
        manifest_bytes,
        pins,
        manifest="conductor/mutation_campaigns/absent.json",
    )
    code, verdict = _run_json(receipt, repo, tree_oid, capsys)
    assert code == EXIT_FAIL
    assert verdict["status"] == "FAIL"
    assert verdict["checks"][PART1]["status"] == "FAIL"
    for name in (PART2, PART3, PART4):
        assert verdict["checks"][name]["status"] == "BLOCKED"
    assert any(
        "part 1" in line and "absent.json" in line for line in verdict["failures"]
    )


def test_part2_manifest_sha256_mismatch_fails(tmp_path, capsys) -> None:
    repo, tree_oid, manifest_bytes, pins = _build_repo(tmp_path)
    tampered_sha = _sha256_hex(manifest_bytes + b"tampered")
    receipt = _write_receipt(
        tmp_path, manifest_bytes, pins, manifest_sha256=tampered_sha
    )
    code, verdict = _run_json(receipt, repo, tree_oid, capsys)
    assert code == EXIT_FAIL
    part2 = verdict["checks"][PART2]
    assert part2["status"] == "FAIL"
    assert part2["recorded"] == tampered_sha
    assert part2["actual"] == _sha256_hex(manifest_bytes)
    # The failure line names both hashes.
    line = next(l for l in verdict["failures"] if "part 2" in l)
    assert tampered_sha in line and _sha256_hex(manifest_bytes) in line
    # Parts 1, 3 and 4 are unaffected by a manifest-hash lie alone.
    assert verdict["checks"][PART1]["status"] == "PASS"
    assert verdict["checks"][PART3]["status"] == "PASS"
    assert verdict["checks"][PART4]["status"] == "PASS"


def test_part3_drifted_source_pin_fails(tmp_path, capsys) -> None:
    repo, _tree, manifest_bytes, pins = _build_repo(tmp_path)
    drifted = b"def add(a, b):\n    return a - b\n"  # genuine content change
    new_tree = _commit_change(repo, "src/module.py", drifted)
    receipt = _write_receipt(tmp_path, manifest_bytes, pins)
    code, verdict = _run_json(receipt, repo, new_tree, capsys)
    assert code == EXIT_FAIL
    part3 = verdict["checks"][PART3]
    assert part3["status"] == "FAIL"
    # Manifest itself did not change, so parts 1-2 still pass.
    assert verdict["checks"][PART1]["status"] == "PASS"
    assert verdict["checks"][PART2]["status"] == "PASS"
    (failure,) = part3["failures"]
    assert "src/module.py" in failure
    assert pins["src/module.py"] in failure  # pinned hash named
    assert _sha256_hex(drifted) in failure  # actual hash named
    # The drift also breaks inventory reproduction, as it must.
    assert verdict["checks"][PART4]["status"] == "FAIL"


def test_part3_missing_pinned_path_fails(tmp_path, capsys) -> None:
    repo, _tree, manifest_bytes, pins = _build_repo(tmp_path)
    new_tree = _commit_change(repo, "tests/test_module.py", None)  # git rm
    receipt = _write_receipt(tmp_path, manifest_bytes, pins)
    code, verdict = _run_json(receipt, repo, new_tree, capsys)
    assert code == EXIT_FAIL
    part3 = verdict["checks"][PART3]
    assert part3["status"] == "FAIL"
    (failure,) = part3["failures"]
    assert "tests/test_module.py" in failure
    assert pins["tests/test_module.py"] in failure
    # With a pinned path unreadable, reproduction is impossible: BLOCKED.
    assert verdict["checks"][PART4]["status"] == "BLOCKED"


def test_part4_receipt_map_divergence_fails_alone(tmp_path, capsys) -> None:
    repo, tree_oid, manifest_bytes, pins = _build_repo(tmp_path)
    # The receipt's own recorded map silently swaps in a hash of other bytes;
    # the manifest in the tree stays pristine, so parts 1-3 all pass.
    lying_pins = dict(pins)
    lying_pins["src/module.py"] = _sha256_hex(b"never these bytes")
    receipt = _write_receipt(tmp_path, manifest_bytes, lying_pins)
    code, verdict = _run_json(receipt, repo, tree_oid, capsys)
    assert code == EXIT_FAIL
    assert verdict["checks"][PART1]["status"] == "PASS"
    assert verdict["checks"][PART2]["status"] == "PASS"
    assert verdict["checks"][PART3]["status"] == "PASS"
    part4 = verdict["checks"][PART4]
    assert part4["status"] == "FAIL"
    assert part4["recorded_digest"] != part4["reproduced_digest"]
    assert part4["divergence"]["hash_mismatch"] == ["src/module.py"]
    assert part4["recorded_digest"] == inventory_digest(lying_pins)
    assert part4["reproduced_digest"] == inventory_digest(pins)


def test_inventory_digest_matches_independent_reference() -> None:
    # Insertion order (and path order) deliberately differ from hash order so
    # a dropped sort produces a different digest. Two entries share identical
    # content (same hash) to pin the sort -k1,1 whole-line tie-break.
    contents = {
        "z/last.py": b"alpha\n",
        "a/first.py": b"omega\n",
        "m/mid.py": b"alpha\n",  # duplicate content: hash tie with z/last.py
    }
    pins = {path: _sha256_hex(data) for path, data in contents.items()}
    reference_lines = sorted(
        (f"{digest}  {path}\n" for path, digest in pins.items()),
        key=lambda line: (line.split("  ", 1)[0], line),
    )
    expected = _sha256_hex("".join(reference_lines).encode("utf-8"))
    assert inventory_digest(pins) == expected
    # Sanity: the chosen fixture really does exercise a reorder.
    unsorted_digest = _sha256_hex(
        "".join(f"{digest}  {path}\n" for path, digest in pins.items()).encode("utf-8")
    )
    assert unsorted_digest != expected


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
