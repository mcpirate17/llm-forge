"""Python surface for the native ablation engine.

The engine itself is Rust (``tooling/native/slop-core``). Everything
here is argument marshalling and a CLI -- deliberately, because parsing and span
rewriting have no business running in the interpreter. Measured over 400 modules of
this repo: 12.80 s in Python for 5,064 ablations, 0.80 s in Rust for 35,372.

There is no pure-Python fallback. A fallback that silently produces a seventh of the
ablations would report a clean sweep that never happened.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import pathlib
import sys

from conductor._native import slop_core

# Decided once when conductor._native loaded: the extension, or SlopCoreUnavailable
# (an ImportError naming the build step). No per-call fallback.
_core = slop_core()


@dataclasses.dataclass(frozen=True)
class Ablation:
    """One construct removed, expressed as byte-span edits against the module source."""

    rule: str
    qualname: str
    line: int
    description: str
    edits: tuple[tuple[int, int, str], ...]

    def apply(self, source: str) -> str:
        return _core.apply(source, list(self.edits))


def rule_names() -> tuple[list[str], list[str]]:
    """``(default, optional)``. Optional rules were measured and never discriminated."""
    default, optional = _core.rule_names()
    return list(default), list(optional)


RULES, OPTIONAL_RULES = rule_names()


def ablations(source: str, rules=None, extra=()) -> list[Ablation]:
    """Every ablation the enabled rules find. Unknown rule names raise KeyError."""
    found = _core.ablations(
        source,
        list(rules) if rules is not None else None,
        list(extra) if extra else None,
    )
    return [
        Ablation(
            rule=a["rule"],
            qualname=a["qualname"],
            line=a["line"],
            description=a["description"],
            edits=tuple(tuple(e) for e in a["edits"]),
        )
        for a in found
    ]


def ablate_module(path, rules=None, extra=()) -> list[tuple[Ablation, str]]:
    """Each ablation of ``path`` paired with the mutated module source."""
    source = pathlib.Path(path).read_text()
    return [(a, a.apply(source)) for a in ablations(source, rules, extra)]


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("path", nargs="?", help="module to ablate")
    ap.add_argument(
        "--rule",
        action="append",
        dest="rules",
        help="restrict to this rule (repeatable)",
    )
    ap.add_argument(
        "--extra",
        action="append",
        default=[],
        help="enable an opt-in rule (repeatable)",
    )
    ap.add_argument("--json", help="write findings to this path")
    ap.add_argument("--list-rules", action="store_true")
    args = ap.parse_args(argv)

    if args.list_rules:
        default, optional = rule_names()
        print(f"default ({len(default)}):")
        for r in default:
            print(f"  {r}")
        print(f"opt-in ({len(optional)}) -- measured, never discriminated:")
        for r in optional:
            print(f"  {r}")
        return 0

    if not args.path:
        ap.error("path is required unless --list-rules")

    found = ablations(pathlib.Path(args.path).read_text(), args.rules, args.extra)
    by_rule: dict[str, int] = {}
    for a in found:
        by_rule[a.rule] = by_rule.get(a.rule, 0) + 1
    for rule, n in sorted(by_rule.items(), key=lambda kv: -kv[1]):
        print(f"  {rule:34s} {n:5d}")
    print(f"{len(found)} ablations in {args.path}")

    if args.json:
        pathlib.Path(args.json).write_text(
            json.dumps([dataclasses.asdict(a) for a in found], indent=2) + "\n"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
