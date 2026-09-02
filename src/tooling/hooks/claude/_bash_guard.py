#!/usr/bin/env python3
"""Command-position deny rules for the PreToolUse/Bash hook.

Reads a shell command on stdin. Exits 0 to allow, 1 to block (reason on stdout).

Why this exists: the previous implementation grepped the raw command string, so
it matched a banned pattern ANYWHERE -- including inside quotes. On 2026-08-08 it
blocked a smoke test whose payload merely *contained* `git push --force` in a
quoted JSON string. Interactively that is a shrug; in an autonomous run it is a
denial with no operator to reinterpret it, and the likely response is a worse
workaround. So matching happens at COMMAND POSITION: the command is tokenized
with shlex (quoted spans collapse into single tokens and can no longer look like
commands), split on shell operators, and each resulting command is matched by its
own argv.

`bash -c "..."` / `sh -c` / `zsh -c` payloads are re-checked recursively, which
closes the obvious bypass that naive command-position matching would open.

If tokenization fails (unbalanced quotes, exotic syntax), it falls back to the
old substring behaviour -- for a deny hook, the safe failure is over-blocking.
"""

from __future__ import annotations

import re
import shlex
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "agent"))
from bash_write_targets import split_commands  # noqa: E402  single definition, shared with crg_gate

OPERATORS = {";", "&&", "||", "|", "&", "\n"}
SHELL_RUNNERS = {"bash", "sh", "zsh", "dash", "ksh"}
RECURSIVE_FLAGS = {"-r", "-R", "--recursive"}

# Fallback patterns, used only when tokenization fails. These are the original
# anywhere-in-string regexes; over-blocking beats under-blocking on a parse error.
FALLBACK_PATTERNS: list[tuple[str, str]] = [
    (r"git\s+push\s+.*(?<!-with-lease)(-f|--force)\b", "git push --force"),
    (r"git\s+reset\s+--hard\b", "git reset --hard"),
    (r"git\s+clean\s+-[fdxX]", "git clean -fd"),
    (r"rm\s+-r[f ]*\s+(/|~/|\.\./|/home)\b", "recursive delete of a dangerous target"),
    (r"^\s*pip\s+install\b", "raw pip install"),
    (r"^\s*python.*-m\s+pip\s+install\b", "raw python -m pip install"),
]

REASONS = {
    "push_force": (
        "BLOCKED: git push --force. Use --force-with-lease if you must, or ask the user."
    ),
    "reset_hard": (
        "BLOCKED: git reset --hard destroys uncommitted work. Stash or commit first."
    ),
    "git_clean": (
        "BLOCKED: git clean deletes untracked files permanently. "
        "Be specific about what to remove."
    ),
    "rm_danger": "BLOCKED: Dangerous recursive delete target.",
    "pip": "BLOCKED: Use 'uv pip install' instead of raw pip.",
}


def _has_force(args: list[str]) -> bool:
    """True for -f/--force, but NOT --force-with-lease.

    The old regex used `(-f|--force)\\b`, and `\\b` matches between the "e" of
    "--force" and the following "-", so `--force-with-lease` was blocked too --
    i.e. the hook rejected the exact remedy its own message recommends.
    """
    return any(
        a == "-f" or (a.startswith("--force") and a != "--force-with-lease")
        for a in args
    )


def check_command(argv: list[str]) -> str | None:
    """Return a block reason for one command's argv, or None to allow."""
    if not argv:
        return None

    exe = argv[0].rsplit("/", 1)[-1]
    args = argv[1:]

    # bash -c "<payload>" — re-check the payload as its own command line.
    if exe in SHELL_RUNNERS and "-c" in args:
        index = args.index("-c")
        if index + 1 < len(args):
            return check(args[index + 1])

    if exe == "git" and args:
        sub = args[0]
        if sub == "push" and _has_force(args[1:]):
            return REASONS["push_force"]
        if sub == "reset" and "--hard" in args[1:]:
            return REASONS["reset_hard"]
        # Only SHORT flags are char-tested: "--dry-run" contains a "d" and must
        # not be mistaken for the destructive -d.
        if sub == "clean" and any(
            (a.startswith("--") and a == "--force")
            or (
                a.startswith("-")
                and not a.startswith("--")
                and set(a[1:]) & set("fdxX")
            )
            for a in args[1:]
        ):
            return REASONS["git_clean"]

    if exe == "rm" and any(
        a in RECURSIVE_FLAGS
        or (a.startswith("-") and not a.startswith("--") and "r" in a.lower())
        for a in args
    ):
        for arg in args:
            if arg.startswith("-"):
                continue
            if arg == "/" or arg.startswith(("/", "~/", "../")):
                return REASONS["rm_danger"]

    if exe == "pip" and args and args[0] == "install":
        return REASONS["pip"]

    if exe.startswith("python") and "-m" in args:
        index = args.index("-m")
        if args[index + 1 : index + 3] == ["pip", "install"]:
            return REASONS["pip"]

    return None


def check(command: str) -> str | None:
    """Return a block reason for a full command line, or None to allow."""
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=True)
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        # Unparseable: fall back to the permissive-parse / aggressive-match path.
        for pattern, label in FALLBACK_PATTERNS:
            if re.search(pattern, command):
                return f"BLOCKED ({label}): command could not be parsed, matched conservatively."
        return None

    for argv in split_commands(tokens):
        reason = check_command(argv)
        if reason:
            return reason
    return None


def main() -> int:
    command = sys.stdin.read()
    reason = check(command)
    if reason:
        print(reason)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
