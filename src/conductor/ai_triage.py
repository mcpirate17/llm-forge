#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import urllib.request
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]


SYSTEM_PROMPT = """You are an AI CI/audit triage engine.
Return JSON only.
You classify findings and choose one action mode:
- comment
- issue
- draft_pr
- manual_only

You must be conservative.
Choose draft_pr only for bounded low-risk fixes.
Choose manual_only for architectural, broad, or unclear problems.
No auto-merge. No destructive advice.
"""


TRIAGE_SCHEMA = {
    "mode": "comment|issue|draft_pr|manual_only",
    "severity": "low|medium|high|critical",
    "title": "short title",
    "summary": "1-3 sentence summary",
    "grouped_findings": [
        {
            "category": "string",
            "items": ["string"],
        }
    ],
    "proposed_actions": ["string"],
    "allowed_patch_scope": ["path or symbol patterns"],
    "tests_to_run": ["command"],
    "body_markdown": "markdown body",
    "dedupe_key": "stable-key",
}


def _load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _default_result(bundle: dict[str, Any], reason: str) -> dict[str, Any]:
    ctx = bundle.get("context", {})
    kind = bundle.get("kind", "unknown")
    title = f"{kind.replace('_', ' ').title()} triage"
    return {
        "mode": "manual_only",
        "severity": "medium",
        "title": title,
        "summary": reason,
        "grouped_findings": [],
        "proposed_actions": ["Review attached workflow artifacts manually."],
        "allowed_patch_scope": [],
        "tests_to_run": [],
        "body_markdown": f"## {title}\n\n{reason}\n",
        "dedupe_key": f"{kind}:{ctx.get('workflow')}:{ctx.get('ref') or ctx.get('sha')}",
        "provider": "none",
    }


def _build_prompt(bundle: dict[str, Any]) -> str:
    return json.dumps(
        {
            "instructions": {
                "respond_json_only": True,
                "schema": TRIAGE_SCHEMA,
                "policy": {
                    "prefer_manual_for_broad_refactors": True,
                    "never_auto_merge": True,
                    "use_issue_for_recurring_or_default_branch_findings": True,
                    "use_comment_for_pr_scoped_findings": True,
                },
            },
            "bundle": bundle,
        },
        indent=2,
    )


def _call_gemini(prompt: str) -> str:
    api_key = os.environ.get("GOOGLE_API_KEY", "")
    model = os.environ.get("GEMINI_MODEL", "gemini-1.5-pro")
    if not api_key:
        raise RuntimeError("GOOGLE_API_KEY is not configured")
    url = (
        f"https://generativelanguage.googleapis.com/v1beta/models/"
        f"{model}:generateContent?key={api_key}"
    )
    payload = {
        "system_instruction": {"parts": [{"text": SYSTEM_PROMPT}]},
        "contents": [{"parts": [{"text": prompt}]}],
        "generationConfig": {
            "temperature": 0.1,
            "responseMimeType": "application/json",
        },
    }
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=90) as resp:
        data = json.loads(resp.read().decode("utf-8"))
    candidates = data.get("candidates") or []
    if not candidates:
        raise RuntimeError("Gemini returned no candidates")
    parts = candidates[0].get("content", {}).get("parts", [])
    text = "".join(
        part.get("text", "") for part in parts if isinstance(part, dict)
    ).strip()
    if not text:
        raise RuntimeError("Gemini returned empty content")
    return text


def _call_openai(prompt: str) -> str:
    api_key = os.environ.get("OPENAI_API_KEY", "")
    model = os.environ.get("OPENAI_MODEL", "gpt-5.4-mini")
    if not api_key:
        raise RuntimeError("OPENAI_API_KEY is not configured")
    url = "https://api.openai.com/v1/chat/completions"
    payload = {
        "model": model,
        "temperature": 0.1,
        "response_format": {"type": "json_object"},
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": prompt},
        ],
    }
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {api_key}",
        },
    )
    with urllib.request.urlopen(req, timeout=90) as resp:
        data = json.loads(resp.read().decode("utf-8"))
    return data["choices"][0]["message"]["content"]


def _call_anthropic(prompt: str) -> str:
    api_key = os.environ.get("ANTHROPIC_API_KEY", "")
    model = os.environ.get("ANTHROPIC_MODEL", "claude-sonnet-4-5-20250929")
    if not api_key:
        raise RuntimeError("ANTHROPIC_API_KEY is not configured")
    url = "https://api.anthropic.com/v1/messages"
    payload = {
        "model": model,
        "max_tokens": 1600,
        "temperature": 0.1,
        "system": SYSTEM_PROMPT,
        "messages": [{"role": "user", "content": prompt}],
    }
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            "x-api-key": api_key,
            "anthropic-version": "2023-06-01",
        },
    )
    with urllib.request.urlopen(req, timeout=90) as resp:
        data = json.loads(resp.read().decode("utf-8"))
    parts = data.get("content") or []
    return "".join(part.get("text", "") for part in parts if isinstance(part, dict))


def _call_provider(provider: str, prompt: str) -> str:
    if provider == "gemini":
        return _call_gemini(prompt)
    if provider == "openai":
        return _call_openai(prompt)
    if provider == "anthropic":
        return _call_anthropic(prompt)
    raise RuntimeError(f"Unsupported provider: {provider}")


def _parse_json_text(text: str) -> dict[str, Any]:
    start = text.find("{")
    end = text.rfind("}")
    if start < 0 or end < start:
        raise ValueError("No JSON object found in model output")
    return json.loads(text[start : end + 1])


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run AI triage against workflow artifacts"
    )
    parser.add_argument("--input", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument(
        "--provider", default=os.environ.get("AI_TRIAGE_PROVIDER", "openai")
    )
    args = parser.parse_args()

    bundle = _load_json(ROOT / args.input)
    prompt = _build_prompt(bundle)
    try:
        raw = _call_provider(args.provider, prompt)
        result = _parse_json_text(raw)
        result["provider"] = args.provider
        result["raw_excerpt"] = raw[:2000]
    except Exception as exc:
        result = _default_result(bundle, f"AI triage unavailable: {exc}")

    output = ROOT / args.output
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
