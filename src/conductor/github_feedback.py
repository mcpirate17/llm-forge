#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import urllib.request
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
MARKER = "<!-- ai-triage-marker -->"


def _load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _request(
    method: str, url: str, token: str, payload: dict[str, Any] | None = None
) -> Any:
    req = urllib.request.Request(
        url,
        method=method,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "Content-Type": "application/json",
        },
        data=json.dumps(payload).encode("utf-8") if payload is not None else None,
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        body = resp.read().decode("utf-8")
        return json.loads(body) if body else {}


def _comment_on_pr(repo: str, pr_number: int, token: str, body: str) -> None:
    url = f"https://api.github.com/repos/{repo}/issues/{pr_number}/comments"
    _request("POST", url, token, {"body": body})


def _upsert_issue(
    repo: str, token: str, title: str, body: str, dedupe_key: str
) -> None:
    search_url = (
        f"https://api.github.com/search/issues?q=repo:{repo}+is:issue+in:body+"
        f'"{dedupe_key}"'
    )
    existing = _request("GET", search_url, token)
    items = existing.get("items") or []
    if items:
        issue_number = items[0]["number"]
        url = f"https://api.github.com/repos/{repo}/issues/{issue_number}"
        _request("PATCH", url, token, {"title": title, "body": body})
    else:
        url = f"https://api.github.com/repos/{repo}/issues"
        _request("POST", url, token, {"title": title, "body": body})


def main() -> int:
    parser = argparse.ArgumentParser(description="Post AI triage feedback to GitHub")
    parser.add_argument("--bundle", required=True)
    parser.add_argument("--triage", required=True)
    args = parser.parse_args()

    bundle = _load_json(ROOT / args.bundle)
    triage = _load_json(ROOT / args.triage)
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN") or ""
    if not token:
        return 0

    ctx = bundle.get("context", {})
    repo = ctx.get("repository")
    if not repo:
        return 0

    body = triage.get("body_markdown") or triage.get("summary") or "AI triage result"
    dedupe_key = (
        triage.get("dedupe_key")
        or f"{bundle.get('kind')}:{ctx.get('workflow')}:{ctx.get('sha')}"
    )
    body = f"{MARKER}\n`dedupe_key`: `{dedupe_key}`\n\n{body}"
    mode = triage.get("mode", "manual_only")

    if mode == "comment" and ctx.get("pr_number"):
        _comment_on_pr(repo, int(ctx["pr_number"]), token, body)
    elif mode == "issue":
        _upsert_issue(repo, token, triage.get("title") or "AI triage", body, dedupe_key)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
