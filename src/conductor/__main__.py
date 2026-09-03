"""``python -m conductor <subcommand>``: the platform's entry point.

Subcommands are thin: each delegates to one module's ``main`` with the remaining argv.
"""

from __future__ import annotations

import sys
from collections.abc import Callable, Sequence

SUBCOMMANDS: dict[str, str] = {
    "init": "conductor.project_init",
}


def _load(module: str) -> Callable[[Sequence[str] | None], int]:
    import importlib

    return importlib.import_module(module).main


def main(argv: Sequence[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if not args or args[0] in {"-h", "--help"}:
        print("usage: python -m conductor <" + "|".join(SUBCOMMANDS) + "> [args...]")
        return 0 if args else 2
    name, rest = args[0], args[1:]
    module = SUBCOMMANDS.get(name)
    if module is None:
        print(f"conductor: unknown subcommand {name!r}", file=sys.stderr)
        return 2
    return _load(module)(rest)


if __name__ == "__main__":
    raise SystemExit(main())
