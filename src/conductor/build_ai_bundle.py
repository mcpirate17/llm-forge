#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]


def _read_text(path: Path, limit: int = 120_000) -> str | None:
    if not path.exists():
        return None
    return path.read_text(encoding="utf-8", errors="ignore")[:limit]


def _load_json(path: Path) -> Any:
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return None


def _event_context() -> dict[str, Any]:
    event_path = os.environ.get("GITHUB_EVENT_PATH", "")
    payload = _load_json(Path(event_path)) if event_path else {}
    payload = payload or {}
    pr = payload.get("pull_request") or {}
    repo = payload.get("repository") or {}
    return {
        "event_name": os.environ.get("GITHUB_EVENT_NAME"),
        "workflow": os.environ.get("GITHUB_WORKFLOW"),
        "job": os.environ.get("GITHUB_JOB"),
        "run_id": os.environ.get("GITHUB_RUN_ID"),
        "run_number": os.environ.get("GITHUB_RUN_NUMBER"),
        "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
        "ref": os.environ.get("GITHUB_REF"),
        "sha": os.environ.get("GITHUB_SHA"),
        "repository": os.environ.get("GITHUB_REPOSITORY") or repo.get("full_name"),
        "server_url": os.environ.get("GITHUB_SERVER_URL", "https://github.com"),
        "pr_number": pr.get("number"),
        "pr_title": pr.get("title"),
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Build AI triage input bundle from workflow artifacts"
    )
    parser.add_argument(
        "--kind", required=True, choices=["governance", "weekly_audit", "pipeline"]
    )
    parser.add_argument("--artifact-dir", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()

    artifact_dir = ROOT / args.artifact_dir
    bundle = {
        "kind": args.kind,
        "context": _event_context(),
        "artifacts": {
            "guardrail_report_markdown": _read_text(
                artifact_dir / "latest_guardrail_report.md"
            ),
            "guardrail_report_json": _load_json(
                artifact_dir / "latest_guardrail_report.json"
            ),
            "profile_hotpaths_json": _load_json(artifact_dir / "profile_hotpaths.json"),
            "research_junit_xml": _read_text(
                artifact_dir / "research-junit.xml", limit=80_000
            ),
            "designer_junit_xml": _read_text(
                artifact_dir / "designer-junit.xml", limit=80_000
            ),
        },
    }
    ctx = bundle["context"]
    if ctx.get("repository") and ctx.get("run_id"):
        bundle["context"]["run_url"] = (
            f"{ctx['server_url']}/{ctx['repository']}/actions/runs/{ctx['run_id']}"
        )

    output = ROOT / args.output
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(bundle, indent=2), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
