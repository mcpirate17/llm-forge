"""Measure the kill rate of *uncurated* mutants against a campaign's own test scope.

A registered campaign ships mutants its author chose. That measures the author's
prediction, not the tests' strength. This harness generates mechanical mutants
(mutmut/cosmic-ray style operators) over the campaign's real subject files,
restricted to lines the campaign's own ranked tests actually execute, and reports
how many the tests catch.

Restricting to covered lines is what separates the two failure modes: a mutant on
an unexecuted line survives because nothing ran it (a coverage gap), while a mutant
on an executed line that survives is a line the tests ran and did not check
(a detection gap). Only the second is evidence about test strength.
"""

from __future__ import annotations

import argparse
import ast
import json
import os
import random
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import ClassVar

from conductor.project_paths import campaigns_root

PY = sys.executable  # the campaign runs under whatever interpreter invoked us


@dataclass
class Mutant:
    path: str
    line: int
    op: str
    detail: str
    source: str


# --------------------------------------------------------------------------- ops


class _Mutator(ast.NodeTransformer):
    """Applies exactly one mutation, selected by index, to a parsed tree."""

    CMP: ClassVar[dict[type[ast.cmpop], type[ast.cmpop]]] = {
        ast.Eq: ast.NotEq,
        ast.NotEq: ast.Eq,
        ast.Lt: ast.LtE,
        ast.LtE: ast.Lt,
        ast.Gt: ast.GtE,
        ast.GtE: ast.Gt,
        ast.Is: ast.IsNot,
        ast.IsNot: ast.Is,
        ast.In: ast.NotIn,
        ast.NotIn: ast.In,
    }
    ARITH: ClassVar[dict[type[ast.operator], type[ast.operator]]] = {
        ast.Add: ast.Sub,
        ast.Sub: ast.Add,
        ast.Mult: ast.Div,
        ast.Div: ast.Mult,
        ast.FloorDiv: ast.Mult,
        ast.Mod: ast.Mult,
    }

    def __init__(self, covered: set[int]) -> None:
        self.covered = covered
        self.sites: list[tuple[str, str, int]] = []  # (op, detail, index)
        self.target = -1
        self.counter = 0
        self.hit: tuple[str, str, int] | None = None

    def _site(self, op: str, detail: str, line: int) -> bool:
        """Record a mutation site; return True if this is the one to apply."""
        if line not in self.covered:
            return False
        idx = self.counter
        self.counter += 1
        if self.target < 0:
            self.sites.append((op, detail, line))
            return False
        if idx == self.target:
            self.hit = (op, detail, line)
            return True
        return False

    # -- visitors

    def visit_Compare(self, node: ast.Compare) -> ast.AST:
        self.generic_visit(node)
        for i, op in enumerate(node.ops):
            swap = self.CMP.get(type(op))
            if swap and self._site(
                "compare", f"{type(op).__name__}->{swap.__name__}", node.lineno
            ):
                node.ops[i] = swap()
        return node

    def visit_BoolOp(self, node: ast.BoolOp) -> ast.AST:
        self.generic_visit(node)
        swap = ast.Or if isinstance(node.op, ast.And) else ast.And
        if self._site(
            "boolop", f"{type(node.op).__name__}->{swap.__name__}", node.lineno
        ):
            node.op = swap()
        return node

    def visit_BinOp(self, node: ast.BinOp) -> ast.AST:
        self.generic_visit(node)
        swap = self.ARITH.get(type(node.op))
        if swap and self._site(
            "arith", f"{type(node.op).__name__}->{swap.__name__}", node.lineno
        ):
            node.op = swap()
        return node

    def visit_AugAssign(self, node: ast.AugAssign) -> ast.AST:
        self.generic_visit(node)
        swap = self.ARITH.get(type(node.op))
        if swap and self._site(
            "augassign", f"{type(node.op).__name__}->{swap.__name__}", node.lineno
        ):
            node.op = swap()
        return node

    def visit_UnaryOp(self, node: ast.UnaryOp) -> ast.AST:
        self.generic_visit(node)
        if isinstance(node.op, ast.Not) and self._site(
            "drop-not", "not X -> X", node.lineno
        ):
            return node.operand
        return node

    def visit_Constant(self, node: ast.Constant) -> ast.AST:
        value = node.value
        if isinstance(value, bool):
            if self._site("const-bool", f"{value}->{not value}", node.lineno):
                return ast.copy_location(ast.Constant(value=not value), node)
        elif isinstance(value, int) and not isinstance(value, bool):
            if self._site("const-int", f"{value}->{value + 1}", node.lineno):
                return ast.copy_location(ast.Constant(value=value + 1), node)
        elif isinstance(value, str):
            new = "" if value else "mutated"
            if self._site("const-str", f"{value[:16]!r}->{new!r}", node.lineno):
                return ast.copy_location(ast.Constant(value=new), node)
        return node

    def visit_Return(self, node: ast.Return) -> ast.AST:
        self.generic_visit(node)
        if node.value is not None and not (
            isinstance(node.value, ast.Constant) and node.value.value is None
        ):
            if self._site("return-none", "return X -> return None", node.lineno):
                return ast.copy_location(ast.Return(value=None), node)
        return node

    def visit_Break(self, node: ast.Break) -> ast.AST:
        if self._site("break-continue", "break -> continue", node.lineno):
            return ast.copy_location(ast.Continue(), node)
        return node

    def visit_Continue(self, node: ast.Continue) -> ast.AST:
        if self._site("continue-break", "continue -> break", node.lineno):
            return ast.copy_location(ast.Break(), node)
        return node


def generate(source: str, path: str, covered: set[int]) -> list[Mutant]:
    """Every single-site mechanical mutant of `source` on a covered line."""

    tree = ast.parse(source)
    probe = _Mutator(covered)
    probe.visit(tree)
    out: list[Mutant] = []
    seen: set[str] = {ast.unparse(ast.parse(source))}
    for index in range(probe.counter):
        mutator = _Mutator(covered)
        mutator.target = index
        mutated = mutator.visit(ast.parse(source))
        if mutator.hit is None:
            continue
        ast.fix_missing_locations(mutated)
        try:
            text = ast.unparse(mutated)
            compile(text, path, "exec")
        except (SyntaxError, ValueError):
            continue
        if text in seen:
            continue
        seen.add(text)
        op, detail, line = mutator.hit
        out.append(Mutant(path=path, line=line, op=op, detail=detail, source=text))
    return out


# ----------------------------------------------------------------------- running


def run_scope(root: Path, argv: list[str], timeout: float) -> tuple[int, str]:
    env = dict(os.environ)
    env["PYTHONPATH"] = str(root)
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    cmd = [PY, *argv[1:], "-p", "no:cacheprovider"]
    try:
        proc = subprocess.run(
            cmd, cwd=root, env=env, capture_output=True, text=True, timeout=timeout
        )
    except subprocess.TimeoutExpired:
        return 124, "TIMEOUT"
    return proc.returncode, (proc.stdout + proc.stderr)[-3000:]


def covered_lines(root: Path, argv: list[str], subjects: list[str], timeout: float):
    """Lines of each subject executed by the campaign's own baseline scope."""

    data = root / ".uncurated_cov"
    report = root / ".uncurated_cov.json"
    env = dict(os.environ)
    env["PYTHONPATH"] = str(root)
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    env["COVERAGE_FILE"] = str(data)
    cmd = [
        PY,
        "-m",
        "coverage",
        "run",
        *[f"--include={s}" for s in subjects],
        "-m",
        "pytest",
        *argv[3:],
        "-p",
        "no:cacheprovider",
    ]
    proc = subprocess.run(
        cmd, cwd=root, env=env, capture_output=True, text=True, timeout=timeout
    )
    if proc.returncode != 0:
        return None, (proc.stdout + proc.stderr)[-2000:]
    proc = subprocess.run(
        [PY, "-m", "coverage", "json", "-o", str(report), "-q"],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        # "No data to report." -- the baseline ran but executed none of the
        # subjects. Nothing to mutate here, and no reason to abandon the
        # sixteen campaigns queued behind this one.
        data.unlink(missing_ok=True)
        return None, (proc.stdout + proc.stderr)[-2000:]
    payload = json.loads(report.read_text())
    out: dict[str, set[int]] = {}
    for name, block in payload["files"].items():
        out[str(Path(name).as_posix())] = set(block["executed_lines"])
    data.unlink(missing_ok=True)
    report.unlink(missing_ok=True)
    return out, ""


@dataclass
class Result:
    campaign: str
    killed: int = 0
    survived: int = 0
    broken: int = 0
    timeout: int = 0
    survivors: list[dict] = field(default_factory=list)
    baseline_seconds: float = 0.0
    generated: int = 0
    ran: int = 0
    # Why a row is empty. A skipped campaign and a campaign that detected
    # nothing both read as 0 killed; only this tells them apart in the table.
    note: str = ""


def measure(
    root: Path,
    manifest_path: Path,
    cap: int,
    seed: int,
    budget: float,
    *,
    name: str | None = None,
) -> Result:
    manifest = json.loads(manifest_path.read_text())
    name = name or manifest_path.stem
    res = Result(campaign=name)
    argv = manifest["baseline"]["argv"]
    subjects = [
        s
        for s in manifest.get("source_sha256", {})
        if s.endswith(".py")
        and "/test_" not in s
        and not Path(s).name.startswith("test_")
    ]
    if not subjects:
        res.note = "no non-test subject files"
        print(f"  {name}: {res.note}, skipped")
        return res

    started = time.monotonic()
    code, out = run_scope(root, argv, timeout=600)
    res.baseline_seconds = time.monotonic() - started
    if code != 0:
        res.note = f"baseline red (exit {code})"
        print(f"  {name}: BASELINE RED (exit {code}), skipped\n{out[-600:]}")
        return res

    cov, err = covered_lines(root, argv, subjects, timeout=900)
    if cov is None:
        res.note = "coverage measurement failed"
        print(f"  {name}: coverage measurement failed, skipped\n{err[-400:]}")
        return res

    originals = {rel: (root / rel).read_text(encoding="utf-8") for rel in subjects}
    for rel, text in originals.items():
        (root / rel).write_text(ast.unparse(ast.parse(text)), encoding="utf-8")
    identity, out = run_scope(root, argv, timeout=600)
    for rel, text in originals.items():
        (root / rel).write_text(text, encoding="utf-8")
    if identity != 0:
        res.note = "ast.unparse round-trip is not behaviour-preserving"
        print(f"  {name}: {res.note}, skipped")
        return res

    mutants: list[Mutant] = []
    for rel in subjects:
        text = (root / rel).read_text(encoding="utf-8")
        mutants.extend(generate(text, rel, cov.get(rel, set())))
    res.generated = len(mutants)
    mutants.sort(key=lambda m: (m.path, m.line, m.op, m.detail))
    if len(mutants) > cap:
        mutants = random.Random(seed).sample(mutants, cap)
        mutants.sort(key=lambda m: (m.path, m.line, m.op, m.detail))

    print(
        f"  {name}: baseline {res.baseline_seconds:.1f}s, "
        f"{res.generated} covered-line mutants, running {len(mutants)} "
        f"(~{len(mutants) * res.baseline_seconds / 60:.1f} min)"
    )

    deadline = time.monotonic() + budget
    try:
        for i, mut in enumerate(mutants, 1):
            if time.monotonic() > deadline:
                print(f"  {name}: budget reached after {i - 1} mutants")
                break
            (root / mut.path).write_text(mut.source, encoding="utf-8")
            code, out = run_scope(root, argv, timeout=max(60, res.baseline_seconds * 8))
            (root / mut.path).write_text(originals[mut.path], encoding="utf-8")
            res.ran += 1
            if code == 124:
                res.timeout += 1  # an infinite loop is a detection, but a noisy one
            elif code == 0:
                res.survived += 1
                res.survivors.append(
                    {
                        "path": mut.path,
                        "line": mut.line,
                        "op": mut.op,
                        "detail": mut.detail,
                    }
                )
            elif "error" in out.lower() and "collected 0 items" in out:
                res.broken += 1
            else:
                res.killed += 1
    finally:
        for rel, text in originals.items():
            (root / rel).write_text(text, encoding="utf-8")
    return res


class CampaignLookupError(ValueError):
    """A requested campaign does not resolve to a manifest in the mutated tree."""


def _index_campaign_ids(campaign_dir: Path) -> dict[str, Path]:
    """Every manifest in the directory, keyed by the id it declares for itself."""

    index: dict[str, Path] = {}
    for path in sorted(campaign_dir.glob("*.json")):
        try:
            declared = json.loads(path.read_text()).get("campaign_id")
        except (OSError, ValueError):
            continue
        if isinstance(declared, str) and declared not in index:
            index[declared] = path
    return index


def resolve_campaigns(root: Path, requested: list[str]) -> list[tuple[str, Path]]:
    """Resolve every requested campaign to a manifest, or refuse the whole run.

    `--campaign` reads as a campaign id, and for most campaigns the id and the
    manifest filename are the same string -- so the two disagree rarely enough that
    the disagreement used to surface as a `FileNotFoundError` raised in the middle
    of the batch. The campaigns before it had been measured, the ones after it never
    were, and the machine time was spent for a partial table. `mutation_value_self_v4`
    is declared in `mutation_value_self.json`, which is exactly that shape.

    So: match the filename first, fall back to the id the manifest declares, and
    refuse up front naming every id that resolved to nothing. Still fail loud -- but
    before the first mutant, not after the sixth campaign.
    """

    campaign_dir = campaigns_root(root)
    resolved: list[tuple[str, Path]] = []
    unresolved: list[str] = []
    by_id: dict[str, Path] | None = None
    for cid in requested:
        direct = campaign_dir / f"{cid}.json"
        if direct.is_file():
            resolved.append((cid, direct))
            continue
        if by_id is None:
            by_id = _index_campaign_ids(campaign_dir)
        found = by_id.get(cid)
        if found is None:
            unresolved.append(cid)
        else:
            resolved.append((cid, found))
    if unresolved:
        raise CampaignLookupError(
            f"{len(unresolved)} campaign(s) resolve to no manifest under "
            f"{campaign_dir}: {', '.join(unresolved)}. Nothing was measured -- "
            "pass the manifest filename or the id the manifest declares."
        )
    return resolved


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--root",
        required=True,
        help="tree to mutate: a disposable worktree, never a live checkout",
    )
    ap.add_argument("--campaign", action="append", required=True)
    ap.add_argument("--cap", type=int, default=40)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--budget-minutes", type=float, default=90.0)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    root = Path(args.root)
    campaigns = resolve_campaigns(root, args.campaign)
    deadline = time.monotonic() + args.budget_minutes * 60
    results = []
    for cid, manifest in campaigns:
        remaining = deadline - time.monotonic()
        if remaining <= 60:
            print(f"global budget exhausted before {cid}")
            break
        res = measure(root, manifest, args.cap, args.seed, remaining, name=cid)
        results.append(res)
        total = res.killed + res.survived + res.timeout
        rate = res.killed / total if total else 0.0
        print(
            f"  {res.campaign}: ran {res.ran} killed {res.killed} survived {res.survived} "
            f"timeout {res.timeout} broken {res.broken} -> kill rate {rate:.0%}"
        )
        Path(args.out).write_text(
            json.dumps([r.__dict__ for r in results], indent=1), encoding="utf-8"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
