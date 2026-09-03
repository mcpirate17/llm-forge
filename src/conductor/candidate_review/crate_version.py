"""Crate version discipline: shipped Rust source may not change silently.

`uv` keys its wheel cache on the distribution version, so a crate whose sources
change without a version bump resolves to the *previous* wheel on the next sync.
Tests then run against the old object code while the tree shows the new source:
the gate passes, and master breaks. That has happened twice on this repo, and
both times cost a full diagnosis cycle because the symptom (a native symbol that
exists in the source and not in the import) points nowhere near the cause.

Two rules, both scoped to crates this candidate actually touches:

* `source-changed-without-version-bump` -- the candidate edits a crate's sources
  but leaves its version equal to the integration base's. Blocking: this is the
  defect itself, and it is free to fix.
* `installed-version-drift` -- the candidate's crate version is not the one
  installed in the interpreter running the gate. The test results in this run
  describe a different build than the tree does, so they cannot be evidence for
  it. Blocking, but only for the lane that changed the crate.
"""

from __future__ import annotations

import time
import tomllib
from importlib.metadata import PackageNotFoundError, version as installed_version
from pathlib import PurePosixPath

from conductor.candidate_review.checks import ReviewContext, _result
from conductor.candidate_review.git_source import run_git
from conductor.candidate_review.model import CheckResult, Finding, Severity

CHECK_ID = "crate-version"
MANIFEST_NAME = "Cargo.toml"
# Suffixes whose change alters the compiled artifact. Cargo.toml is deliberately
# absent: it is the file carrying the bump, so counting it would demand a second
# bump for the commit that performs the first.
BUILD_INPUT_SUFFIXES = frozenset({".rs", ".c", ".cc", ".cpp", ".cu", ".h", ".hpp"})
# Cargo compiles these directories only under `cargo test`/`cargo bench`, so their
# contents never reach the shipped wheel and cannot be served stale from its cache.
NON_ARTIFACT_DIRS = frozenset({"tests", "benches", "examples"})
TEST_ATTRIBUTE = "#[cfg(test)]"


def _strip_test_modules(text: str) -> str | None:
    """`text` with every `#[cfg(test)]` item removed, or None if that is unsafe.

    Measured on 40 commits of master: without this, 4 of the 6 candidates the rule
    fired on were adding inline `#[cfg(test)]` tests, which change no shipped byte.
    A gate that is wrong two times out of three gets routed around rather than
    obeyed, so the false positives have to go before the rule can block.

    Returns None when the braces do not balance -- an unparseable file falls back
    to demanding the bump, which is the safe direction to be wrong in.
    """

    out: list[str] = []
    index = 0
    length = len(text)
    while True:
        start = text.find(TEST_ATTRIBUTE, index)
        if start == -1:
            out.append(text[index:])
            return "".join(out)
        out.append(text[index:start])
        cursor = start + len(TEST_ATTRIBUTE)
        depth = 0
        opened = False
        while cursor < length:
            char = text[cursor]
            if char == '"':
                cursor = _skip_string(text, cursor)
                continue
            if char == "/" and text.startswith("//", cursor):
                cursor = text.find("\n", cursor)
                if cursor == -1:
                    return None
                continue
            if char == "/" and text.startswith("/*", cursor):
                end = text.find("*/", cursor + 2)
                if end == -1:
                    return None
                cursor = end + 2
                continue
            if char == "{":
                depth += 1
                opened = True
            elif char == "}":
                depth -= 1
                if depth == 0:
                    cursor += 1
                    break
                if depth < 0:
                    return None
            elif char == ";" and not opened:
                # An attribute on a non-block item (`#[cfg(test)] use ...;`).
                cursor += 1
                break
            cursor += 1
        else:
            return None
        index = cursor


def _skip_string(text: str, cursor: int) -> int:
    """Index just past the string literal whose opening quote is at `cursor`.

    A raw literal is recognised backwards: the scanner dispatches on the quote,
    so `r` and its hashes are already behind the cursor by the time we arrive.
    Reading them forwards never matches, and an unescaped `"` inside `r#"..."#`
    then reopens the scan mid-literal.
    """

    hashes = 0
    probe = cursor - 1
    while probe >= 0 and text[probe] == "#":
        hashes += 1
        probe -= 1
    if probe >= 0 and text[probe] == "r":
        terminator = '"' + "#" * hashes
        end = text.find(terminator, cursor + 1)
        return len(text) if end == -1 else end + len(terminator)
    cursor += 1
    while cursor < len(text):
        if text[cursor] == "\\":
            cursor += 2
            continue
        if text[cursor] == '"':
            return cursor + 1
        cursor += 1
    return cursor


def _manifest_dirs(ctx: ReviewContext) -> dict[str, str]:
    """Crate directory -> manifest path, over the candidate tree."""

    manifests: dict[str, str] = {}
    for entry in ctx.entries:
        path = PurePosixPath(entry.path)
        if path.name != MANIFEST_NAME:
            continue
        manifests[str(path.parent) if path.parent.parts else ""] = entry.path
    return manifests


def _owning_crate(rel: str, crate_dirs: list[str]) -> str | None:
    """The deepest crate directory containing `rel`.

    Deepest, not first: a workspace root carries a Cargo.toml too, and attributing
    a member's source to the root would demand a bump on the wrong manifest.
    """

    path = PurePosixPath(rel)
    best: str | None = None
    for crate in crate_dirs:
        prefix = PurePosixPath(crate) if crate else PurePosixPath(".")
        parts = () if crate == "" else prefix.parts
        if path.parts[: len(parts)] != parts:
            continue
        if best is None or len(crate) > len(best):
            best = crate
    return best


def _package_table(text: str) -> dict[str, object]:
    try:
        parsed = tomllib.loads(text)
    except (tomllib.TOMLDecodeError, ValueError):
        return {}
    package = parsed.get("package")
    return package if isinstance(package, dict) else {}


def _base_text(ctx: ReviewContext, rel: str) -> str | None:
    """`rel` as it stands on the integration base, or None if absent there."""

    completed = run_git(
        ctx.repo,
        ["show", f"{ctx.candidate.base_tree_oid}:{rel}"],
        check=False,
    )
    if completed.returncode != 0:
        return None
    return completed.stdout.decode("utf-8", "replace")


def _reaches_the_artifact(ctx: ReviewContext, crate: str, rel: str) -> bool:
    """Whether this changed source can alter the crate's shipped build.

    Two ways it cannot: the file lives in a directory Cargo builds only for
    `cargo test`, or every line it changed sits inside a `#[cfg(test)]` item.
    Anything else -- including a file the stripper cannot parse -- counts, so an
    unreadable source demands the bump rather than waiving it.
    """

    relative = PurePosixPath(rel).relative_to(crate) if crate else PurePosixPath(rel)
    if relative.parts and relative.parts[0] in NON_ARTIFACT_DIRS:
        return False
    if PurePosixPath(rel).suffix.lower() != ".rs":
        return True
    base = _base_text(ctx, rel)
    if base is None:
        return True
    try:
        head = (ctx.snapshot / rel).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return True
    stripped_base = _strip_test_modules(base)
    stripped_head = _strip_test_modules(head)
    if stripped_base is None or stripped_head is None:
        return True
    return stripped_base != stripped_head


def _touched_crates(ctx: ReviewContext, crate_dirs: list[str]) -> set[str]:
    touched: set[str] = set()
    for change in ctx.live_changes:
        if PurePosixPath(change.path).suffix.lower() not in BUILD_INPUT_SUFFIXES:
            continue
        crate = _owning_crate(change.path, crate_dirs)
        if crate is None:
            continue
        if _reaches_the_artifact(ctx, crate, change.path):
            touched.add(crate)
    return touched


def _finding(rule_id: str, severity: Severity, path: str, message: str) -> Finding:
    return Finding(
        check_id=CHECK_ID,
        rule_id=rule_id,
        severity=severity,
        path=path,
        message=message,
    )


def check_crate_version(ctx: ReviewContext) -> CheckResult:
    started = time.perf_counter()
    manifests = _manifest_dirs(ctx)
    crate_dirs = sorted(manifests)
    touched = _touched_crates(ctx, crate_dirs)
    findings: list[Finding] = []

    for crate in sorted(touched):
        manifest = manifests[crate]
        try:
            text = (ctx.snapshot / manifest).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            findings.append(
                _finding(
                    "unreadable-manifest",
                    Severity.CRITICAL,
                    manifest,
                    "crate sources changed but the manifest could not be read, so no "
                    "version bump can be proven",
                )
            )
            continue
        package = _package_table(text)
        current = package.get("version")
        name = package.get("name")
        if not isinstance(current, str):
            findings.append(
                _finding(
                    "unresolvable-crate-version",
                    Severity.HIGH,
                    manifest,
                    "crate sources changed and [package] version is not a literal "
                    "string, so the wheel cache key cannot be shown to have moved",
                )
            )
            continue

        base_text = _base_text(ctx, manifest)
        if base_text is not None:
            base = _package_table(base_text).get("version")
            if isinstance(base, str) and base == current:
                findings.append(
                    _finding(
                        "source-changed-without-version-bump",
                        Severity.CRITICAL,
                        manifest,
                        f"crate sources changed but the version stayed {current}; "
                        "uv keys its wheel cache on the version, so the next sync "
                        "serves the previous build and every test in this run "
                        "measures the old object code",
                    )
                )

        if not isinstance(name, str):
            continue
        try:
            found = installed_version(name)
        except PackageNotFoundError:
            continue
        if found != current:
            findings.append(
                _finding(
                    "installed-version-drift",
                    Severity.HIGH,
                    manifest,
                    f"crate declares {current} but {name} {found} is installed in "
                    "the interpreter running this gate; rebuild before treating "
                    "this run as evidence for the candidate",
                )
            )

    findings.sort(key=lambda item: (item.path or "", item.rule_id))
    return _result(
        CHECK_ID,
        started,
        findings,
        files=sorted(manifests[crate] for crate in touched),
        metrics={"crates_indexed": len(manifests), "crates_touched": len(touched)},
    )
