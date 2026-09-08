"""Reject hand-authored mutation campaigns.

A campaign whose mutants an agent writes by hand is an agent scoring the damage
it chose to do. `mutation_engine_generated` records what that is worth: 482 of
483 scored campaigns publish exactly 1.0, while a mechanical sweep of the same
code kills 60.6%. A curated corpus measures the curator, not the tests.

So a *new* campaign manifest must come from a generator. This check reads the
changed files a review hands it and refuses any manifest that both declares an
engine `conductor.mutation_engine_generated` cannot run and did not exist at the
review's base commit.

That last clause is the whole scope of the rule, and it is deliberate. The 542
hand-written campaigns already committed need their pinned hashes repinned every
time the source under them moves -- PR #375 repinned three in the course of
fixing an unrelated venv bug. A rule that made every such repin conditional on
converting the campaign first would be paid around, not obeyed. Editing a
hand-written campaign therefore stays legal; writing a new one does not, and
deleting one is the cure rather than an offence.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import PurePosixPath

CAMPAIGN_ROOT = PurePosixPath("conductor/mutation_campaigns")

USAGE = "usage: check_generated_mutants --repo REPO --base COMMIT [PATH ...]"


def generated_engines() -> frozenset[str]:
    """The engines the runner can actually execute, read from the runner.

    Imported here rather than restated so this set can never drift from the
    adapters it is supposed to name; the import costs about 10 ms.
    """

    from conductor.mutation_engine_generated import GENERATED_ENGINES

    return GENERATED_ENGINES


def declared_engine(path: str, text: str) -> str | None:
    """The hand-authored engine `path` declares, or None if it declares none.

    Anything that is not a campaign manifest sitting directly in the campaign
    directory is None: `registry.json` and `reproducibility_baseline.json` live
    there too and are told apart by carrying no `campaign_id`, while receipts
    and patches live in subdirectories.
    """

    candidate = PurePosixPath(path)
    if candidate.parent != CAMPAIGN_ROOT or candidate.suffix != ".json":
        return None
    try:
        payload = json.loads(text)
    except json.JSONDecodeError:
        return None
    if not isinstance(payload, dict) or "campaign_id" not in payload:
        return None
    engine = payload.get("mutation_engine")
    if not isinstance(engine, str):
        return "<none declared>"
    return None if engine in generated_engines() else engine


def paths_at_base(repo: str, base: str, paths: list[str]) -> frozenset[str]:
    """The subset of `paths` that already existed at `base`.

    One `ls-tree` for the whole set rather than a `cat-file` each, and a bad
    base ref raises here instead of being read as "every file is new".
    """

    if not paths:
        return frozenset()
    completed = subprocess.run(
        ["git", "-C", repo, "ls-tree", "-r", "--name-only", "-z", base, "--", *paths],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"git ls-tree {base} failed: {completed.stderr.strip() or 'no output'}"
        )
    return frozenset(entry for entry in completed.stdout.split("\0") if entry)


def parse_argv(argv: list[str]) -> tuple[str, str, list[str]]:
    """Split `--repo`/`--base` off the front of the changed-file list.

    argparse is not used because every remaining argument is a path and one of
    them can begin with a dash on a tree nobody has forbidden that on yet.
    """

    repo = base = None
    rest = list(argv)
    while len(rest) >= 2 and rest[0] in ("--repo", "--base"):
        if rest[0] == "--repo":
            repo = rest[1]
        else:
            base = rest[1]
        rest = rest[2:]
    if repo is None or base is None:
        raise SystemExit(USAGE)
    return repo, base, rest


def main() -> int:
    repo, base, paths = parse_argv(sys.argv[1:])
    engines: list[tuple[str, str]] = []
    for path in paths:
        try:
            with open(path, encoding="utf-8") as handle:
                text = handle.read()
        except FileNotFoundError:
            # guardrail: allow-fallback -- a review hands this check the paths its
            # DIFF names, and a diff that deletes a campaign names a path the tree
            # no longer holds. Deleting a hand-written campaign is the cure this
            # rule wants, so the absent file is passed over rather than failing the
            # check that asked for the deletion. Narrowed to FileNotFoundError on
            # purpose: a permission fault or a bad encoding is a real failure and
            # still propagates.
            continue
        engine = declared_engine(path, text)
        if engine is not None:
            engines.append((path, engine))
    if not engines:
        return 0
    existing = paths_at_base(repo, base, [path for path, _ in engines])
    bad = [(path, engine) for path, engine in engines if path not in existing]
    for path, engine in bad:
        print(
            f"new hand-authored mutation campaign: {path} declares "
            f"mutation_engine={engine} - generate it with "
            f"`python -m conductor.mutation_campaign_generate write` "
            f"(engines: {', '.join(sorted(generated_engines()))}). "
            f"Editing a campaign that already existed at {base[:12]} is allowed; "
            f"adding a new hand-written one is not.",
            file=sys.stderr,
        )
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
