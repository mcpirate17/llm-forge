# Native guard and external-invariant contract migration

This cohort moves the behavior assertions from 21 named cases in
`src/conductor/test_current_work_guard.py` and 13 in
`src/conductor/test_external_invariants.py` into two Rust-owned PyO3 test
targets. The original Python baseline passed 34/34 before migration. The
Python originals were retired after independent parity review, target
registration, and successful native validation. The baseline log is
`/tmp/forge-guard-invariants-python-baseline.log` (34 passed); the run set
`PYTHONDONTWRITEBYTECODE=1` and `CUDA_VISIBLE_DEVICES=''`.

`python_contracts_current_work_guard.rs` calls the shipped
`conductor.current_work_guard` and `conductor.local_ai_policy` APIs. It covers
file-path and shell denials, harmless shell mentions, local-agent authority,
Grok and Codex response shapes, test-writing advisories, malformed input,
and the CLI's silent success. Its only child process invokes the shipped
`conductor.current_work_guard` module on fixed JSON input. It writes a
`.current_work.md` fixture only inside an isolated temporary directory.

`python_contracts_external_invariants.rs` calls the shipped
`conductor.candidate_review.external_invariants.evaluate` API. Rust stubs
`_installed_torch_version` and `_measure`, preserving the original callbacks'
zero- and four-argument signatures. Each measurement checks the scratch
paths, `import`/`call` label, pytest arguments, and call count. The fixtures
cover line-level import subtraction, a mapped native artifact, explicit
measurement error, `subprocess.TimeoutExpired`, malformed declarations,
gating, justification, and full torch-version pinning. No torch import,
coverage subprocess, model job, or network access occurs.

Registered sources and their consumers:

| Shipped source or entry point | Rust target | Consumer |
| --- | --- | --- |
| `src/conductor/current_work_guard.py` | `python_contracts_current_work_guard` | `src/tooling/hooks/dispatch/adapters.py`, `src/tooling/hooks/claude/pre-edit.sh`, `src/conductor/conftest.py` |
| `src/conductor/local_ai_policy.py` | `python_contracts_current_work_guard` | current-work guard's local-agent denial |
| `src/conductor/candidate_review/external_invariants.py` | `python_contracts_external_invariants` | `src/conductor/candidate_review/verification.py` |
| `native/conductor-native/tests/python_contracts/support.rs` | both targets | isolated environment, Python module loading, attribute restoration |

Exact source-to-target registry rows for the parent migration lane:

```text
src/conductor/current_work_guard.py	python_contracts_current_work_guard
src/conductor/local_ai_policy.py	python_contracts_current_work_guard
src/conductor/candidate_review/external_invariants.py	python_contracts_external_invariants
native/conductor-native/tests/python_contracts/support.rs	python_contracts_current_work_guard
native/conductor-native/tests/python_contracts/support.rs	python_contracts_external_invariants
```

The test targets use only temporary fixtures and restore patched Python
attributes. Repository searches found no production imports of either
original Python test module. There are no new source-language algorithms or
dependencies on the retired tests or on `conftest.py`.

Both Rust targets passed with `python-compat-tests` enabled: 21/21 guard
cases and 13/13 invariant cases
(`/tmp/forge-guard-invariants-cargo-test.log`). Scoped Clippy with
`-D warnings` passed (`/tmp/forge-guard-invariants-clippy.log`). Cargo ran
offline with at most two build jobs and CUDA hidden.
