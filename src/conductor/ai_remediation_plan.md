# AI Remediation Plan

## Scope
Advisory-first AI triggering for:

- `governance-ci`
- `weekly-audit`
- `pipeline-ci`

The AI layer consumes workflow artifacts, emits structured triage JSON, and
optionally posts PR comments or opens/updates GitHub issues. It does not edit
the repository or auto-merge changes in this phase.

## Trigger Model

### Governance CI
- Trigger after `static-governance`.
- Purpose: summarize blocking static findings and recommend bounded next steps.
- Default action:
  - PR: comment
  - Push/default branch: issue or manual-only

### Weekly Audit
- Trigger after `audit-and-profile`.
- Purpose: produce a weekly architecture/governance summary from audit artifacts.
- Default action:
  - open or update a GitHub issue with a stable marker

### Pipeline CI
- Trigger after the main contract jobs complete.
- Purpose: summarize runtime/observability failures, correlate artifact payloads,
  and recommend the next action.
- Default action:
  - PR: comment
  - push/default branch: issue for repeated failures, otherwise manual-only

## Output Contract
AI emits a single JSON object with:

- `mode`: `comment|issue|draft_pr|manual_only`
- `severity`: `low|medium|high|critical`
- `title`
- `summary`
- `grouped_findings`
- `proposed_actions`
- `allowed_patch_scope`
- `tests_to_run`
- `body_markdown`
- `dedupe_key`

## Safety Rules
- No auto-merge.
- No repo mutation in this phase.
- `draft_pr` may be emitted by the model but workflows do not act on it yet.
- `manual_only` is the fallback when:
  - no provider credentials exist
  - artifact bundle is incomplete
  - the issue scope is architectural or high-risk

## Supported Providers
- `openai` (default)
- `anthropic`
- `gemini`

Configured by environment:

- `AI_TRIAGE_PROVIDER`
- provider-specific API key/model env vars

Recommended default:
- `AI_TRIAGE_PROVIDER=openai`
- `OPENAI_MODEL=gpt-5.4-mini`

## Next Phase
Once triage quality is stable:
- enable issue dedupe by `dedupe_key`
- allow draft PR generation for a fix whitelist
- keep patch application on a trusted runner only
