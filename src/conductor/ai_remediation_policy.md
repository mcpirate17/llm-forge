# AI Remediation Policy & Specification (v1.0)

## 1. Architecture for AI Triggering
The system operates as a post-processor for existing CI workflows. AI is never the primary executor; it is an analyst that consumes artifacts.

*   **Trigger Mechanism:** GitHub Actions `workflow_run` (on completion of Governance/Pipeline CI) or `schedule` (for Weekly Audit).
*   **Data Flow:**
    1.  **Workflow A (Generator):** Runs `guardrail_audit.py` or `profile_hotpaths.py`. Uploads JSON artifacts.
    2.  **Workflow B (AI Orchestrator):** Downloads artifacts -> Filters by **Deduplication Key** (Hash of `file_path + violation_type + context`) -> Invokes AI Agent.
    3.  **Persistence:** Uses a `manifest.json` stored in the `tasks/audit/` directory (or a dedicated `ai-state` branch) to track "Seen/Ignored/In-Progress" violations.

## 2. Phased Rollout Plan
*   **Phase 1: Ghost Critic (Advisory Only):** AI posts PR comments or opens GitHub Issues with "AI-Suggested" labels. No code changes.
*   **Phase 2: Draft Engineer (Safe Fixes):** AI opens **Draft PRs** for whitelisted classes (Dead code, imports, LOC splitting). Requires human "Ready for Review" toggle.
*   **Phase 3: Performance Architect (Native Rewrites):** AI generates Cython/Rust/Vectorized implementations in side-car files (`module_native.pyx`) for hotspots identified in profiling.

## 3. Decision Matrix: Automated Actions

| Violation Category | Severity | Action (Main Branch) | Action (PR Branch) |
| :--- | :--- | :--- | :--- |
| **Dead Code / Unused Imports** | Low | Open Draft PR | Post PR Comment |
| **God File (>1250 LOC)** | Med | Open Issue + Draft Split | Post PR Comment |
| **God Function (>100 LOC)** | Med | Open Issue | Post PR Comment |
| **Runtime Failure (CI)** | High | Open Issue + Triage Report | Post PR Comment |
| **Hotspot (Python Loop)** | Med | Open Issue + Native Suggestion | Post PR Comment |
| **Hotspot (Critical Path)** | High | Open Draft PR (Native Rewrite) | N/A (Requires Audit) |

## 4. Whitelisted Autonomous Fix Classes
*   **Dead Code Removal:** Deleting functions/classes flagged by `vulture` with 100% confidence.
*   **Import Optimization:** Removing unused imports or sorting via `isort`/`ruff`.
*   **Type Hint Injection:** Adding PEP 484 hints to untyped internal functions.
*   **Vectorization:** Replacing standard Python loops with NumPy equivalents.
*   **Surgical Refactoring:** Extracting sub-functions from "God Functions" to reduce LOC.

## 5. Blacklisted Autonomous Fix Classes
*   **Business Logic:** Core `research/` algorithms or `aria_designer/` state management.
*   **Security/Auth:** `keys_DO_NOT_DELETE.txt`, `*auth*`, or `*permission*` files.
*   **Database Schema:** SQL or migration files.
*   **Concurrency:** `async`, `threading`, or `multiprocessing` logic.

## 6. Output JSON Schema: AI Triage
```json
{
  "triage_id": "sha256_hash",
  "category": "GOVERNANCE | PERFORMANCE | RUNTIME",
  "severity": "LOW | MEDIUM | HIGH | CRITICAL",
  "target": {
    "file_path": "string",
    "symbol": "string",
    "line_range": [start, end]
  },
  "finding": "string",
  "action_type": "COMMENT | ISSUE | DRAFT_PR | ESCALATE",
  "deduplication_key": "string"
}
```

## 7. Output JSON Schema: AI Draft-Patch Planning
```json
{
  "plan_id": "sha256_hash",
  "rationale": "string",
  "impact_metrics": {
    "loc_delta": "number",
    "expected_perf_gain": "string | null"
  },
  "patches": [
    {
      "file_path": "string",
      "instruction": "string",
      "old_content": "string",
      "new_content": "string"
    }
  ],
  "verification_commands": ["string"]
}
```

## 8. Operational Risks & Mitigations
*   **Hallucinations:** Draft PRs *must* include automated property-based tests comparing original vs. rewrite.
*   **Oscillations:** Limit to 3 automated remediation attempts per violation before escalating to `MANUAL_REVIEW`.
*   **Context Exhaustion:** Orchestrator must only feed the target symbol and immediate dependencies.
