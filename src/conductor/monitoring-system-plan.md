# Monitoring System Plan: Strict Adherence to GLOBAL_DEV_PROMPT.md

## Objective
Design and implement an automated monitoring system that guarantees 100% adherence to `GLOBAL_DEV_PROMPT.md`. The primary goal is to ruthlessly enforce performance, simplicity, and maintainability, with a specific, aggressive mandate to push the use of C, C++, Rust, Cython, and vectorized approaches in favor of plain Python or JS/TS for computationally intensive tasks.

## Scope & Impact
This system will act as a continuous, automated "elite performance-minded staff engineer." It will intercept, analyze, and optionally block code at multiple stages (pre-commit, PR review, and continuous auditing) to ensure zero tolerance for bloat, dead code, god files/functions, and inappropriate language choices.

## Proposed Solution

The monitoring system will be composed of four primary enforcement layers:

### 1. Pre-commit and Static Enforcement (The Baseline)
*   **AST & Metric Checks:** Implement strict git pre-commit hooks that fail locally if files exceed 1250 lines or functions exceed 100 lines.
*   **Dead Code Elimination:** Integrate tools like `vulture` (for Python) and `ts-prune`/`eslint` (for TS/JS) into the CI pipeline to strictly block merges containing unused functions, classes, methods, or imports.
*   **Complexity Scans:** Fail builds on high cyclomatic complexity or deep nesting, preventing "god functions" from ever being committed.

### 2. Continuous Profiling & Language Enforcer (The "Native" Push)
*   **Automated Profiling:** Run benchmarks and profilers (e.g., `scalene`, `py-spy`) continuously in the CI/CD pipeline against key workflows.
*   **Language Upgrade Triggers:** If a CPU-bound hot path is identified as running in plain Python or JS, the system will automatically open an issue/PR mandating a rewrite in **Rust, C++, C, Cython**, or using vectorized libraries (NumPy/SciPy). 
*   **Anti-Pattern Detection:** Use AST scanners to detect plain Python `for` loops over large numeric arrays or repeated object creation in hot paths, directly flagging them for native/vectorized rewrites.

### 3. LLM-Powered PR Gatekeeper (The Reviewer)
*   **Contextual AI Review:** Deploy an AI agent in the CI pipeline that evaluates every PR against the rules in `GLOBAL_DEV_PROMPT.md`.
*   **Strict Blocking:** The bot will block PRs that introduce speculative abstractions, unbounded caches, N+1 query patterns, or unnecessary sync code.
*   **Rewrite Proposals:** The bot will aggressively suggest or mandate C/C++/Rust/Cython alternatives whenever a developer submits performance-critical logic in Python.

### 4. Scheduled Deep Audit Engine (The Overseer)
*   **Weekly Audits:** A cron job that runs a comprehensive codebase scan, acting as the "ruthless architecture critic."
*   **Standardized Output:** It will automatically generate and publish reports formatted exactly as required by the prompt:
    *   *A. Critical problems*
    *   *B. Exact targets*
    *   *C. Fast wins*
    *   *D. Structural rewrites*
    *   *E. Performance upgrades by language (Highlighting Rust/C/C++/Cython opportunities)*
    *   *F. Proposed patch plan*
    *   *G. Proof*

## Implementation Steps
1.  **Set up the Ruleset:** Configure `.pre-commit-config.yaml` with custom bash/python scripts to enforce the 1250/100 LOC limits and integrate dead-code scanners (`vulture`, etc.).
2.  **Develop the Profiling CI Action:** Create a GitHub Action that runs `scalene`/benchmark scripts on PRs, failing the check if Python execution time exceeds a threshold without a native extension fallback.
3.  **Deploy the LLM Gatekeeper:** Implement a lightweight script that feeds the PR diff and `GLOBAL_DEV_PROMPT.md` to an LLM, returning a pass/fail and a critique. Integrate this as a required status check on GitHub.
4.  **Build the Audit Engine:** Create the scheduled job that orchestrates static analysis, profiling results, and LLM evaluation to generate the `Audit Report` Markdown artifact.

## Verification & Testing
*   **Negative Testing:** Submit intentional "bad" PRs (e.g., a 1300-line file, a pure Python matrix multiplication loop, unused functions) and verify the CI/Gatekeeper blocks them.
*   **Audit Verification:** Trigger a manual run of the Audit Engine and verify the output markdown strictly adheres to sections A through G.
*   **Language Push Verification:** Ensure the Gatekeeper successfully flags inefficient Python loops and demands a Rust/Cython implementation.