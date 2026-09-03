"""Concise console, SARIF, and JUnit reporting for review receipts."""

from __future__ import annotations

import xml.etree.ElementTree as ET
from pathlib import Path

from conductor.candidate_review.model import ReviewReceipt, write_json_atomic
from conductor.candidate_review.value_waivers import WAIVED_RULE


def human_summary(receipt: ReviewReceipt) -> str:
    # WAIVED lines are printed in full and ahead of everything else: they are the only
    # reason a blocking finding did not block, and a developer who cannot see them
    # cannot tell a waived gate from an unarmed one.
    waived = [
        finding for finding in receipt.findings if finding["rule_id"] == WAIVED_RULE
    ]
    blocking = [
        finding
        for finding in receipt.findings
        if finding["severity"] in {"critical", "high"}
        and not finding.get("exception_id")
        and finding not in waived
    ]
    advisory = [
        finding
        for finding in receipt.findings
        if finding not in blocking
        and finding not in waived
        and not finding.get("exception_id")
    ]
    cached = receipt.cache.get("hits", 0)
    duration = receipt.timings.get("duration_ms", 0)
    lines = [
        (
            f"candidate-review {receipt.decision.upper()} | {receipt.surface}/{receipt.profile} | "
            f"tree {str(receipt.candidate['tree_oid'])[:12]} | {duration} ms | {cached} cache hits"
        ),
        f"blocking={len(blocking)} advisory={len(advisory)} waived={len(waived)} "
        f"receipt={receipt.receipt_id}",
    ]
    shown = [*waived, *blocking, *advisory][: 20 + len(waived)]
    for finding in shown:
        location = finding.get("path") or "governance"
        if finding.get("line"):
            location = f"{location}:{finding['line']}"
        exception = (
            f" [excepted:{finding['exception_id']}]"
            if finding.get("exception_id")
            else ""
        )
        lines.append(
            f"- {str(finding['severity']).upper()} {finding['check_id']}/{finding['rule_id']} "
            f"{location}: {finding['message']}{exception}"
        )
    if len(receipt.findings) > len(shown):
        lines.append(
            f"- ... {len(receipt.findings) - len(shown)} more findings in the JSON receipt"
        )
    return "\n".join(lines) + "\n"


def sarif_payload(receipt: ReviewReceipt) -> dict[str, object]:
    rules: dict[str, dict[str, object]] = {}
    results: list[dict[str, object]] = []
    levels = {
        "critical": "error",
        "high": "error",
        "medium": "warning",
        "low": "note",
        "info": "note",
    }
    for finding in receipt.findings:
        rule_id = f"{finding['check_id']}/{finding['rule_id']}"
        rules.setdefault(
            rule_id,
            {
                "id": rule_id,
                "shortDescription": {"text": str(finding["rule_id"])},
                "help": {"text": str(finding.get("help") or finding["message"])},
            },
        )
        result: dict[str, object] = {
            "ruleId": rule_id,
            "level": levels[str(finding["severity"])],
            "message": {"text": str(finding["message"])},
            "partialFingerprints": {
                "governanceFingerprint": str(finding["fingerprint"])
            },
            "properties": {
                "exceptionId": finding.get("exception_id"),
                "candidateTree": receipt.candidate["tree_oid"],
            },
        }
        if finding.get("path"):
            region: dict[str, int] = {}
            if finding.get("line"):
                region["startLine"] = int(finding["line"])
            if finding.get("column") is not None:
                region["startColumn"] = int(finding["column"]) + 1
            result["locations"] = [
                {
                    "physicalLocation": {
                        "artifactLocation": {"uri": str(finding["path"])},
                        **({"region": region} if region else {}),
                    }
                }
            ]
        results.append(result)
    return {
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [
            {
                "tool": {
                    "driver": {
                        "name": "LLM Candidate Review",
                        "informationUri": "https://github.com/",
                        "rules": list(rules.values()),
                    }
                },
                "automationDetails": {"id": receipt.receipt_id},
                "results": results,
            }
        ],
    }


def junit_xml(receipt: ReviewReceipt) -> str:
    suite = ET.Element(
        "testsuite",
        {
            "name": f"candidate-review:{receipt.surface}:{receipt.profile}",
            "tests": str(len(receipt.checks)),
            "failures": str(
                sum(check["status"] in {"failed", "error"} for check in receipt.checks)
            ),
            "skipped": str(
                sum(check["status"] == "skipped" for check in receipt.checks)
            ),
            "time": f"{float(receipt.timings['duration_ms']) / 1000:.3f}",
        },
    )
    suite.set("id", receipt.receipt_id)
    for check in receipt.checks:
        case = ET.SubElement(
            suite,
            "testcase",
            {
                "name": str(check["check_id"]),
                "classname": "governance.candidate_review",
                "time": f"{float(check['duration_ms']) / 1000:.3f}",
            },
        )
        if check["status"] == "skipped":
            ET.SubElement(
                case,
                "skipped",
                {"message": str(check.get("skipped_reason") or "skipped")},
            )
        elif check["status"] in {"failed", "error"}:
            messages = "\n".join(
                f"{finding['severity']} {finding['rule_id']}: {finding['message']}"
                for finding in check.get("findings", [])
            )
            failure = ET.SubElement(
                case, "failure", {"message": f"{check['check_id']} failed"}
            )
            failure.text = messages
        output = ET.SubElement(case, "system-out")
        output.text = str(check.get("stdout_tail") or "")
        error = ET.SubElement(case, "system-err")
        error.text = str(check.get("stderr_tail") or "")
    return ET.tostring(suite, encoding="unicode", xml_declaration=True) + "\n"


def write_outputs(
    receipt: ReviewReceipt,
    *,
    json_out: Path | None,
    sarif_out: Path | None,
    junit_out: Path | None,
) -> None:
    if json_out:
        write_json_atomic(json_out, receipt.to_dict())
    if sarif_out:
        write_json_atomic(sarif_out, sarif_payload(receipt))
    if junit_out:
        junit_out.parent.mkdir(parents=True, exist_ok=True)
        temporary = junit_out.with_name(f".{junit_out.name}.tmp")
        temporary.write_text(junit_xml(receipt), encoding="utf-8")
        temporary.replace(junit_out)


def write_failure_json(path: Path, payload: dict[str, object]) -> None:
    write_json_atomic(path, payload)
