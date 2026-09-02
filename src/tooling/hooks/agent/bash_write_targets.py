#!/usr/bin/env python3
"""Extract the repo paths a shell command would write.

Why this exists: the graph+claim gate (`crg_gate.py verify`) is registered for
`Edit|Write|NotebookEdit` only. A `Bash` call that redirects a heredoc into a
tracked file, or runs `sed -i`, performs exactly the same mutation with none of
the enforcement -- so "graph before edit" and "claim before edit" were one tool
choice away from being optional. This module supplies the missing half: given a
command line, it names the repo-relative paths the command would modify, so the
gate can demand the same claim it demands of `Edit`.

Matching happens at COMMAND POSITION, reusing the tokenizer doctrine from
`.claude/hooks/_bash_guard.py`: heredoc bodies are stripped first (their content
is data, not commands, and unbalanced quotes inside them break tokenization),
then shlex splits the remainder and each operator-delimited command is inspected
by its own argv.

KNOWN LIMIT, deliberately not papered over: an interpreter that computes its
target at runtime (`python - <<EOF` building a path from a variable) cannot be
resolved statically. Rather than let that be a silent hole, an interpreter whose
inline source contains a write idiom is reported via `OPAQUE_WRITE` -- the gate
turns that into a deny telling the agent to use `Write`/`Edit`, which is the
tool that was supposed to be used anyway.
"""

from __future__ import annotations

import re
import shlex
from pathlib import Path

#: Token boundaries between commands. Shell keywords are here because a loop body
#: (`for f in ...; do cp "$f" "$d"; done`) otherwise runs on into the next command
#: and the destination-operand rules read the wrong token as a path.
OPERATORS = {
    ";",
    "&&",
    "||",
    "|",
    "&",
    "\n",
    "do",
    "done",
    "then",
    "fi",
    "else",
    "elif",
    "esac",
    "{",
    "}",
}
SHELL_RUNNERS = {"bash", "sh", "zsh", "dash", "ksh"}

#: Sentinel target for a write whose path is not statically knowable.
OPAQUE_WRITE = "<opaque-interpreter-write>"

#: argv[0] values that write every non-flag operand.
_WRITE_ALL_OPERANDS = {"rm", "truncate", "touch", "shred", "unlink"}

#: argv[0] values that write only their LAST operand (the destination).
_WRITE_LAST_OPERAND = {"cp", "mv", "install", "ln", "rsync"}

#: Short flags that consume the following argument, per command. Without this,
#: `truncate -s 0 log.txt` reports "0" as a write target and the gate demands a
#: claim on a path that cannot exist. Only the commands in the write sets above
#: need an entry; long `--flag=value` forms carry their value already.
_VALUE_FLAGS: dict[str, frozenset[str]] = {
    "truncate": frozenset({"-s", "-r", "--size", "--reference"}),
    "shred": frozenset({"-n", "-s", "--iterations", "--size", "--random-source"}),
    "install": frozenset({"-m", "-o", "-g", "-t", "--mode", "--owner", "--group"}),
    "cp": frozenset({"-t", "-S", "--target-directory", "--suffix"}),
    "mv": frozenset({"-t", "-S", "--target-directory", "--suffix"}),
    "ln": frozenset({"-t", "-S", "--target-directory", "--suffix"}),
    "rsync": frozenset({"-e", "--rsh", "--exclude", "--include", "--files-from"}),
    "tee": frozenset({"-p", "--output-error"}),
}

#: Interpreters whose inline source may perform writes we cannot resolve.
_INTERPRETERS = {"python", "python3", "perl", "ruby", "node", "php"}

#: Python/Perl/Ruby idioms that mutate the filesystem.
_INLINE_WRITE_IDIOMS = re.compile(
    r"""
    open\s*\([^)]*['"][wax]b?\+?['"]      # open(path, "w"/"a"/"x")
    | \.write_text\s*\(
    | \.write_bytes\s*\(
    | \.unlink\s*\(
    | \.mkdir\s*\(
    | \.rename\s*\(
    | \.replace\s*\(\s*[^)]*Path
    | os\.(?:remove|unlink|rename|replace|truncate|makedirs)\s*\(
    | shutil\.(?:copy|copy2|copyfile|copytree|move|rmtree)\s*\(
    | json\.dump\s*\(
    | \bFile::(?:Copy|Path)\b
    """,
    re.VERBOSE,
)

#: Path literals bound to an actual WRITE, not merely mentioned.
#:
#: Collecting every `Path("...")` over-reports badly: a script that reads three
#: paths and writes one reported all four, and one that merely mentioned
#: `Path("<the repo root>")` reported the repo root, which the gate then
#: denied as a write to ".". Only a literal that is the receiver of a write, or
#: the destination argument of one, counts. `write_text` takes the *content*, so
#: it is never itself a source of a path.
_WRITE_METHODS = "write_text|write_bytes|unlink|mkdir|rename|touch"

#: `name = Path("literal")` -- counted only if `name` is written later.
_PATH_BINDING = re.compile(
    r"""([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(?:\w+\.)?Path\s*\(\s*['"]([^'"]+)['"]""",
)

#: `name.write_text(...)` -- the receiver of a write.
_WRITTEN_NAME = re.compile(rf"([A-Za-z_][A-Za-z0-9_]*)\.(?:{_WRITE_METHODS})\s*\(")

#: Literals written with no intermediate binding.
_DIRECT_WRITE_LITERAL = re.compile(
    rf"""
    (?:\w+\.)?Path\s*\(\s*['"]([^'"]+)['"]\s*\)\s*\.(?:{_WRITE_METHODS})\s*\(
    | open\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"][wax]
    | os\.(?:remove|unlink|rename|replace|truncate|makedirs)\s*\(\s*['"]([^'"]+)['"]
    | shutil\.(?:copy|copy2|copyfile|copytree|move|rmtree)\s*\([^,)]*,\s*['"]([^'"]+)['"]
    """,
    re.VERBOSE,
)

#: Redirect operators that OPEN A FILE. `>&` duplicates a descriptor and is
#: deliberately absent; `&>` sends both streams to a file and belongs here.
_WRITE_REDIRECTS = {">", ">>", "&>", "&>>"}

#: Redirect targets that are not files worth claiming.
_NULL_SINKS = {"/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"}

_HEREDOC = re.compile(r"<<-?\s*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1")

#: `NAME=value` assignments, used to expand variables in redirect targets.
_ASSIGNMENT = re.compile(
    r"""(?:^|[;&|\n]|\s)([A-Za-z_][A-Za-z0-9_]*)=(?:"([^"]*)"|'([^']*)'|([^\s;&|]+))""",
)

#: `for NAME in v1 v2 ...; do` -- the loop list is right there in the command, so
#: the variable is resolvable and the bulk-copy idiom need not be denied.
_FOR_LOOP = re.compile(
    r"\bfor\s+([A-Za-z_][A-Za-z0-9_]*)\s+in\s+([^;\n]+?)\s*(?:;|\n)\s*do\b",
)

#: `$NAME` / `${NAME}` references inside a path.
_VARIABLE = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)")

#: Write-shaped operators used by the unparseable-command fallback.
_FALLBACK_WRITE_SHAPE = re.compile(
    r"(^|[^0-9<>&])>{1,2}\s*[^&\s]|(^|\s)(sed\s+-[a-z]*i|tee|dd\s+of=)\b",
)


def separate_lines(command: str) -> str:
    """Replace unquoted newlines with `;` so each line stays its own command.

    `shlex(whitespace_split=True)` treats a newline as ordinary whitespace and
    never emits it as a token, so `"\\n"` in OPERATORS was dead and two commands
    on consecutive lines merged into one argv. A merged argv is not a harmless
    over-read: the destination rules take the LAST operand, so
    `cp a b` followed by `git -C x status` reported "status" as a write target.
    """
    out: list[str] = []
    quote = ""
    index = 0
    while index < len(command):
        char = command[index]
        if quote:
            out.append(char)
            if char == quote:
                quote = ""
            elif char == "\\" and quote == '"' and index + 1 < len(command):
                index += 1
                out.append(command[index])
        elif char == "\\" and index + 1 < len(command):
            out.append(char)
            index += 1
            out.append(command[index])
        elif char in "'\"":
            quote = char
            out.append(char)
        elif char == "\n":
            out.append(";")
        else:
            out.append(char)
        index += 1
    return "".join(out)


def _heredoc_delimiters(line: str, quote: str) -> tuple[list[str], str]:
    """Heredoc delimiters introduced OUTSIDE a quoted span, plus the exit state.

    `<<` inside a quoted argument is data, not a redirection. A CLI whose payload
    merely mentions one -- `handoff append --body "... python3 - <<EOF ..."` --
    otherwise made the scanner hunt for a terminator that never comes, swallow the
    rest of the command, and then deny it on the unparseable fallback. Quote state
    carries across lines because the payload is usually a multi-line argument.
    """
    delimiters: list[str] = []
    index = 0
    while index < len(line):
        char = line[index]
        if quote:
            if char == quote:
                quote = ""
            elif char == "\\" and quote == '"':
                index += 1
        elif char in "'\"":
            quote = char
        elif char == "\\":
            index += 1
        elif line.startswith("<<", index):
            match = _HEREDOC.match(line, index)
            if match:
                delimiters.append(match.group(2))
                index = match.end() - 1
        index += 1
    return delimiters, quote


def split_heredocs(command: str) -> tuple[str, list[tuple[str, str]]]:
    """Split `command` into (command without heredoc bodies, [(intro, body)]).

    `cat > f <<'EOF' ... EOF` must still expose its `> f` redirect, but the body
    is arbitrary text that would otherwise be tokenized as commands. The bodies
    are returned rather than discarded because a body fed to an interpreter is
    program text, not data -- see `_heredoc_write_targets`.
    """
    lines = command.split("\n")
    kept: list[str] = []
    bodies: list[tuple[str, str]] = []
    quote = ""
    index = 0
    while index < len(lines):
        intro = lines[index]
        kept.append(intro)
        delimiters, quote = _heredoc_delimiters(intro, quote)
        index += 1
        for delimiter in delimiters:
            body: list[str] = []
            while index < len(lines) and lines[index].strip() != delimiter:
                body.append(lines[index])
                index += 1
            index += 1  # consume the terminator itself
            bodies.append((intro, "\n".join(body)))
    return "\n".join(kept), bodies


def _heredoc_write_targets(bodies: list[tuple[str, str]]) -> list[str]:
    """Write targets hidden in a heredoc that is piped into an interpreter.

    `python3 - <<'EOF' ... Path(x).write_text(...) ... EOF` is the single most
    direct way to edit a tracked file without touching `Edit`/`Write`, and it is
    invisible to argv inspection because the program text never appears in argv.
    A heredoc introduced by `cat`/`tee`/anything else is data and is skipped.
    """
    targets: list[str] = []
    for intro, body in bodies:
        first = intro.strip().split()
        if not first:
            continue
        exe = first[0].rsplit("/", 1)[-1]
        if exe in SHELL_RUNNERS:
            targets.extend(write_targets(body, _relative=False))
            continue
        if exe.rstrip("0123456789.") not in {
            name.rstrip("0123456789.") for name in _INTERPRETERS
        }:
            continue
        if not _INLINE_WRITE_IDIOMS.search(body):
            continue
        literals = _path_literals(body)
        targets.extend(literals if literals else [OPAQUE_WRITE])
    return targets


def _assignments(command: str) -> dict[str, str]:
    """`NAME=value` bindings made by the command itself."""
    bindings: dict[str, str] = {}
    for match in _ASSIGNMENT.finditer(command):
        name, double, single, bare = match.groups()
        bindings[name] = next(v for v in (double, single, bare) if v is not None)
    return bindings


def _loop_bindings(command: str, bindings: dict[str, str]) -> dict[str, list[str]]:
    """`for NAME in a b c` bindings, when every element resolves to a literal.

    A loop whose list is computed (`for f in $(git ls-files)`) stays unresolved
    and its writes report opaque, but `for f in a b c; do cp ... ; done` -- the
    bulk-copy idiom agents actually use -- resolves to one target per value
    rather than being denied wholesale. `for f in $FILES` counts as literal when
    the command sets FILES itself, which is the same idiom with the list hoisted
    into a variable.
    """
    loops: dict[str, list[str]] = {}
    for match in _FOR_LOOP.finditer(command):
        listing = _VARIABLE.sub(
            lambda m: bindings.get(m.group(1) or m.group(2), "\0"), match.group(2)
        )
        values = listing.split()
        if any("\0" in v or "$" in v or "*" in v or "`" in v for v in values):
            continue
        loops[match.group(1)] = [value.strip("\"'") for value in values]
    return loops


def _expand_all(
    target: str, bindings: dict[str, str], loops: dict[str, list[str]]
) -> list[str]:
    """Every path `target` can denote, expanding loop variables to their values."""
    pending = [target]
    for name, values in loops.items():
        if not any(
            f"${name}" in candidate or f"${{{name}}}" in candidate
            for candidate in pending
        ):
            continue
        pending = [
            candidate.replace(f"${{{name}}}", value).replace(f"${name}", value)
            for candidate in pending
            for value in values
        ]
    return [_expand(candidate, bindings) for candidate in pending]


def _expand(target: str, bindings: dict[str, str]) -> str:
    """Substitute `$VAR` in a redirect target, or return OPAQUE_WRITE.

    shlex does not expand variables, so `SP=/tmp/x; echo hi > $SP/f.txt` reached
    the claim check as the literal string "$SP/f.txt" and resolved *inside the
    repo* -- a false denial of a scratchpad write. Bindings made by the command
    itself cover that idiom; a variable from the surrounding environment cannot
    be resolved here, and an unresolved path is reported opaque rather than
    guessed at.
    """
    if "$" not in target:
        return target
    resolved = _VARIABLE.sub(
        lambda m: bindings.get(m.group(1) or m.group(2), "\0"), target
    )
    return OPAQUE_WRITE if "\0" in resolved else resolved


def _path_literals(source: str) -> list[str]:
    """Literal paths this source actually WRITES, in first-seen order.

    A path that is only read, or passed as an argument, is not a target: over-
    reporting them turned an ordinary edit script into a denial on the repo root.
    """
    found: dict[str, None] = {}
    for match in _DIRECT_WRITE_LITERAL.finditer(source):
        for group in match.groups():
            if group:
                found.setdefault(group, None)
    bindings = {m.group(1): m.group(2) for m in _PATH_BINDING.finditer(source)}
    for match in _WRITTEN_NAME.finditer(source):
        bound = bindings.get(match.group(1))
        if bound:
            found.setdefault(bound, None)
    return list(found)


def split_commands(tokens: list[str]) -> list[list[str]]:
    """Split a flat token stream into individual commands at shell operators."""
    commands: list[list[str]] = []
    current: list[str] = []
    for token in tokens:
        if token in OPERATORS:
            if current:
                commands.append(current)
            current = []
        else:
            current.append(token)
    if current:
        commands.append(current)
    return commands


def _redirect_targets(argv: list[str]) -> list[str]:
    """Paths written by a file-opening redirect inside one command's argv.

    Membership, not a prefix test, decides this. shlex groups punctuation, so
    `2>&1` tokenizes as `2`, `>&`, `1`: the descriptor-duplication forms `>&`
    and `>&2` are distinct tokens that simply are not in the set, while `&>`
    (stdout+stderr into a file) genuinely is a write and would otherwise pass
    unseen.
    """
    targets: list[str] = []
    for position, token in enumerate(argv):
        if token not in _WRITE_REDIRECTS:
            continue
        if position + 1 >= len(argv):
            continue
        targets.append(argv[position + 1])
    return targets


def _strip_redirects(argv: list[str]) -> list[str]:
    """Drop redirect operators and their operands from an argv.

    Heredoc introducers count: `split_heredocs` removes the BODY but leaves the
    `<<` and its delimiter in the command line, so `tee out.txt <<'EOF'` reported
    a write to a file called "EOF" alongside the real one.
    """
    cleaned: list[str] = []
    skip = False
    for token in argv:
        if skip:
            skip = False
            continue
        if token in _WRITE_REDIRECTS or token in {"<", ">&", "&>", "<<", "<<-"}:
            skip = True
            continue
        cleaned.append(token)
    return cleaned


def _operands(args: list[str], exe: str = "") -> list[str]:
    """Non-flag arguments, stopping flag parsing at `--`.

    `exe` matters because a short flag may consume the argument after it:
    `truncate -s 0 log.txt` otherwise reports "0" as a write target, and the
    gate then demands a claim on a path that cannot exist.
    """
    takes_value = _VALUE_FLAGS.get(exe, frozenset())
    operands: list[str] = []
    flags_done = False
    skip = False
    for arg in args:
        if skip:
            skip = False
            continue
        if not flags_done and arg == "--":
            flags_done = True
            continue
        if not flags_done and arg.startswith("-") and arg != "-":
            skip = arg in takes_value
            continue
        operands.append(arg)
    return operands


def _inline_source(exe: str, args: list[str]) -> str | None:
    """The inline program text of an interpreter invocation, if any."""
    if exe.rsplit("/", 1)[-1].rstrip("0123456789.") not in {
        name.rstrip("0123456789.") for name in _INTERPRETERS
    }:
        return None
    for flag in ("-c", "-e"):
        if flag in args:
            index = args.index(flag)
            if index + 1 < len(args):
                return args[index + 1]
    return None


def _command_targets(argv: list[str]) -> list[str]:
    """Write targets for a single command's argv."""
    if not argv:
        return []

    targets = _redirect_targets(argv)
    argv = _strip_redirects(argv)
    if not argv:
        return targets

    exe = argv[0].rsplit("/", 1)[-1]
    args = argv[1:]

    if exe in SHELL_RUNNERS and "-c" in args:
        index = args.index("-c")
        if index + 1 < len(args):
            targets.extend(write_targets(args[index + 1], _relative=False))
        return targets

    inline = _inline_source(exe, args)
    if inline is not None and _INLINE_WRITE_IDIOMS.search(inline):
        literals = _path_literals(inline)
        targets.extend(literals if literals else [OPAQUE_WRITE])
        return targets

    if exe in _WRITE_ALL_OPERANDS:
        targets.extend(_operands(args, exe))
    elif exe in _WRITE_LAST_OPERAND:
        operands = _operands(args, exe)
        if len(operands) >= 2:
            targets.append(operands[-1])
    elif (
        exe == "sed"
        and any(
            arg.startswith("-") and not arg.startswith("--") and "i" in arg[1:]
            for arg in args
        )
        or (exe == "sed" and any(arg.startswith("--in-place") for arg in args))
    ):
        # `sed -i` rewrites every file operand; the script itself is operand 0
        # unless it was supplied with -e/-f.
        operands = _operands(args)
        targets.extend(
            operands if any(a in {"-e", "-f"} for a in args) else operands[1:]
        )
    elif exe == "tee":
        targets.extend(_operands(args, exe))
    elif exe == "dd":
        targets.extend(arg[3:] for arg in args if arg.startswith("of="))
    elif exe == "git" and args:
        sub = args[0]
        if sub in {"apply", "checkout", "restore", "stash"}:
            targets.extend(_operands(args[1:]))
    elif exe == "patch":
        targets.extend(_operands(args))

    return targets


def write_targets(command: str, *, _relative: bool = True) -> list[str]:
    """Return the paths `command` would write, in first-seen order.

    Returns `[OPAQUE_WRITE]` for a command that is write-shaped but whose target
    cannot be resolved statically.
    """
    stripped, bodies = split_heredocs(command)
    found = _heredoc_write_targets(bodies)
    stripped = separate_lines(stripped)
    try:
        lexer = shlex.shlex(stripped, posix=True, punctuation_chars=True)
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        # Unparseable. Deny only if it *looks* like a write: blanket-denying
        # every exotic command line would break far more than it protects.
        if _FALLBACK_WRITE_SHAPE.search(stripped):
            found.append(OPAQUE_WRITE)
        tokens = []

    for argv in split_commands(tokens):
        found.extend(_command_targets(argv))

    bindings = _assignments(stripped)
    loops = _loop_bindings(stripped, bindings)
    seen: dict[str, None] = {}
    for target in found:
        for expanded in _expand_all(target, bindings, loops):
            if expanded not in _NULL_SINKS:
                seen.setdefault(expanded, None)
    return list(seen)


def working_directory(command: str, repo_root: Path) -> Path | None:
    """The directory a relative write target resolves against.

    `cd /tmp/scratch && ... > note.txt` writes to /tmp, but a hook that resolves
    every relative path against the repo root reads that as a repo write and
    denies it -- the second false denial this gate produced on its first day.
    Returns None when a `cd` target cannot be resolved, so the caller can treat
    relative targets as opaque instead of resolving them wrongly.
    """
    stripped, _ = split_heredocs(command)
    bindings = _assignments(stripped)
    stripped = separate_lines(stripped)
    try:
        lexer = shlex.shlex(stripped, posix=True, punctuation_chars=True)
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        return repo_root
    current = repo_root
    for argv in split_commands(tokens):
        if not argv or argv[0].rsplit("/", 1)[-1] != "cd":
            continue
        operands = _operands(argv[1:])
        if not operands:
            return None  # `cd` with no argument goes home; not resolvable here
        expanded = _expand(operands[0], bindings)
        if expanded == OPAQUE_WRITE or expanded == "-":
            return None
        candidate = Path(expanded)
        current = candidate if candidate.is_absolute() else current / candidate
    return current


def repo_write_targets(command: str, repo_root: Path) -> list[str]:
    """Write targets that land inside `repo_root`, as repo-relative paths.

    Paths outside the repo (the session scratchpad, `/tmp`) have nothing to
    claim and are dropped. `OPAQUE_WRITE` is preserved verbatim.
    """
    base = working_directory(command, repo_root)
    resolved: list[str] = []
    for target in write_targets(command):
        if target == OPAQUE_WRITE:
            resolved.append(target)
            continue
        path = Path(target)
        if not path.is_absolute() and base is None:
            resolved.append(OPAQUE_WRITE)
            continue
        absolute = path if path.is_absolute() else (base / path)
        try:
            resolved.append(absolute.resolve().relative_to(repo_root).as_posix())
        except ValueError:
            continue
    return resolved
