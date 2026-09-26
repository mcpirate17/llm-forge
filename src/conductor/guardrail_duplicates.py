"""Global Pylint similarity audit with native indexed candidate selection.

Pylint retains normalization, pair comparison, thresholds and grouping. Rust
only rules out pairs that cannot share a minimum similarity window. This avoids
re-hashing every unrelated file for every other file in a whole-tree audit.
"""

from __future__ import annotations

import configparser
import io
import subprocess
import sys
import tokenize
from collections.abc import Callable, Generator, Sequence
from dataclasses import asdict, dataclass
from pathlib import Path
from time import perf_counter

import astroid
from astroid.exceptions import AstroidError
from pylint.checkers.symilar import Commonality, LineSet, Symilar
from pylint.lint import PyLinter
from pylint.utils import tokenize_module
from pylint.utils.file_state import FileState

from conductor._native import guardrail_duplicate_candidates_native


@dataclass(frozen=True)
class DuplicateScan:
    findings: tuple[str, ...]
    files: int
    candidate_pairs: int
    possible_pairs: int
    indexed_windows: int
    elapsed_seconds: float
    normalization: dict[str, bool]


@dataclass(frozen=True)
class SimilarityOptions:
    ignore_comments: bool
    ignore_docstrings: bool
    ignore_imports: bool
    ignore_signatures: bool


def _configured_options(root: Path) -> SimilarityOptions:
    """Ask Pylint to resolve host configuration without running source checks.

    The original audit's CLI overrides only the enabled message and threshold.
    Generating its effective config preserves Pylint's config search, plugin and
    init-hook handling in the selected host, without process-global chdir.
    """
    command = [
        sys.executable,
        "-m",
        "pylint",
        "--disable=all",
        "--enable=duplicate-code",
        "--min-similarity-lines=10",
        "--generate-rcfile",
    ]
    try:
        result = subprocess.run(
            command,
            cwd=root,
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        raise ValueError(
            "Pylint host configuration did not resolve within 15s"
        ) from exc
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()[:500]
        raise ValueError(
            f"Pylint host configuration failed ({result.returncode}): {detail}"
        )
    config = configparser.ConfigParser(interpolation=None)
    try:
        config.read_string(result.stdout)
        return SimilarityOptions(
            **{
                name: config.getboolean("SIMILARITIES", name.replace("_", "-"))
                for name in SimilarityOptions.__dataclass_fields__
            }
        )
    except (configparser.Error, ValueError) as exc:
        raise ValueError(
            f"Pylint effective similarity configuration is invalid: {exc}"
        ) from exc


class _IndexedSymilar(Symilar):
    candidate_count = 0
    indexed_windows = 0

    def _iter_sims(self) -> Generator[Commonality, None, None]:
        pairs, self.indexed_windows, _ = guardrail_duplicate_candidates_native(
            [
                [line.text for line in lineset.stripped_lines]
                for lineset in self.linesets
            ],
            self.namespace.min_similarity_lines,
        )
        self.candidate_count = len(pairs)
        for first, second in pairs:
            yield from self._find_common(self.linesets[first], self.linesets[second])


def _line_enabled(source: str, target: str) -> Callable[[str, int], bool] | None:
    """Use Pylint's own scoped pragma handling when a file contains directives."""
    if "pylint:" not in source:
        return None
    linter = PyLinter()
    linter.load_default_plugins()
    linter.disable("all")
    linter.enable("duplicate-code")
    node = astroid.parse(source, module_name=target)
    linter.set_current_module(target, target)
    linter.file_state = FileState(target, linter.msgs_store, node=node)
    linter.process_tokens(tokenize_module(node))
    if linter._ignore_file:
        return lambda _message, _line: False
    return linter._is_one_message_enabled


def scan_duplicates(root: Path, targets: Sequence[str]) -> DuplicateScan:
    """Compare the complete selected set using the host's effective ignore rules."""
    started = perf_counter()
    options = asdict(_configured_options(root))
    checker = _IndexedSymilar(min_lines=10, **options)
    for target in sorted(set(targets)):
        try:
            with tokenize.open(root / target) as stream:
                source = stream.read()
            callback = _line_enabled(source, target)
            if callback is None:
                checker.append_stream(target, io.StringIO(source))
            else:
                checker.linesets.append(
                    LineSet(
                        target,
                        source.splitlines(keepends=True),
                        **options,
                        line_enabled_callback=callback,
                    )
                )
        except (AstroidError, SyntaxError, UnicodeError, tokenize.TokenError) as exc:
            raise ValueError(f"cannot normalize {target}: {exc}") from exc
    findings = []
    for count, occurrences in checker._compute_sims():
        locations = sorted(
            f"{lineset.name}:{start + 1}-{end}" for lineset, start, end in occurrences
        )
        findings.append(
            f"R0801: {count} similar lines in {len(locations)} files: "
            + ", ".join(locations)
            + " (duplicate-code)"
        )
    total = len(checker.linesets)
    return DuplicateScan(
        findings=tuple(sorted(findings)),
        files=total,
        candidate_pairs=checker.candidate_count,
        possible_pairs=total * (total - 1) // 2,
        indexed_windows=checker.indexed_windows,
        elapsed_seconds=perf_counter() - started,
        normalization=options,
    )
