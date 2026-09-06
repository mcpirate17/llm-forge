"""Coverage accounting for checks that read files off the candidate snapshot.

A scan that skips an unreadable file and carries on reports exactly the same
PASS as a scan that read every byte it was asked to. The check's coverage
silently becomes a function of which files happened to be readable, the receipt
records nothing about it, and a reviewer has no way to tell a clean scan from a
scan that never looked. This module makes the difference legible: every scan
declares what it expected to read, what it read, and what it could not, and the
counts ride out on the CheckResult whether or not anything was skipped.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

from conductor.candidate_review.model import Finding, Severity


@dataclass
class ScanLedger:
    """What one filesystem scan was asked to read, and what it actually got."""

    check_id: str
    expected: list[str] = field(default_factory=list)
    read: list[str] = field(default_factory=list)
    # Relative path -> the exception that stopped the read, as "Type: message".
    skipped: dict[str, str] = field(default_factory=dict)

    def read_text(self, root: Path, rel: str) -> str | None:
        """Read one file, recording the outcome either way.

        Returns None when the file could not be read, so callers keep the shape
        of the `continue` they had; the difference is that the skip is now on
        the record instead of vanishing.
        """

        self.expected.append(rel)
        try:
            source = (root / rel).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as exc:
            self.skipped[rel] = f"{type(exc).__name__}: {exc}"
            return None
        self.read.append(rel)
        return source

    def metrics(self) -> dict[str, object]:
        """Coverage counts for the receipt, emitted on every run.

        Reported unconditionally: "0 skipped" is the evidence that the scan was
        complete, and it only means that if the field is always there.
        """

        return {
            "files_expected": len(self.expected),
            "files_read": len(self.read),
            "files_skipped": len(self.skipped),
            "skipped_files": dict(sorted(self.skipped.items())),
        }

    def incomplete_findings(
        self,
        *,
        adjudicated: set[str],
        severity: Severity,
        subject: str,
        corpus_severity: Severity = Severity.MEDIUM,
    ) -> list[Finding]:
        """One finding per unreadable file, weighted by what the skip costs.

        A file the check is responsible for adjudicating -- one in the candidate
        change set -- is fatal to the verdict: the check cannot say anything
        about the code under review, so it fails closed at `severity` rather
        than passing on an unexamined file. A file elsewhere in the corpus only
        weakens the comparison set, which can cost a true finding but never
        invents a false one, so it is reported at `corpus_severity` and left
        advisory. Failing the whole repo closed on an unreadable file nobody
        touched would hand any single bad byte a veto over every candidate.
        """

        findings: list[Finding] = []
        for rel, reason in sorted(self.skipped.items()):
            changed = rel in adjudicated
            findings.append(
                Finding(
                    check_id=self.check_id,
                    rule_id=(
                        "unreadable-changed-file" if changed else "incomplete-scan"
                    ),
                    severity=severity if changed else corpus_severity,
                    path=rel,
                    message=(
                        f"{subject} could not be read ({reason}); "
                        + (
                            "this file is in the candidate change set, so the "
                            "check cannot adjudicate the code under review"
                            if changed
                            else "the comparison corpus is incomplete and the "
                            "check may miss a real finding"
                        )
                    ),
                    help=(
                        "Restore the file, or exclude it from the scan "
                        "deliberately rather than by failing to open it."
                    ),
                    evidence={"reason": reason, "in_change_set": changed},
                )
            )
        return findings
