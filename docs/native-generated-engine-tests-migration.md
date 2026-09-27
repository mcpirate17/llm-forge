# Generated mutation engine contracts

`src/conductor/test_mutation_engine_generated.py` contained 29 named tests.
The isolated Python baseline passed 29/29
(`/tmp/forge-generated-engine-baseline.log`). The Rust-owned PyO3 replacement
keeps every named case across three targets:

| Target | Cases | Contract |
| --- | ---: | --- |
| `python_contracts_mutation_generated_core` | 13 | Scope refusal, mutant identity, scoring, timeout and survivor ratchet |
| `python_contracts_mutation_generated_manifest` | 9 | Manifest fields, defaults, engine adapter resolution and command pinning |
| `python_contracts_mutation_generated_receipt` | 7 | Receipt paths, baseline errors and compact disk encoding |

The fixtures create only temporary manifest JSON, receipt JSON and one toy
source file. The direct-run case replaces `changed_sources` and `adapter_for`
with Rust callbacks, then asserts the scope refusal and adapter name. A Rust
trap on `isolated_snapshot` makes worktree creation an immediate test failure.
No test starts a mutation engine, builds a snapshot or claims campaign evidence.
All scoring and receipt assertions are in Rust; the tests call the existing
production APIs through PyO3.

Independent parity and consumer review passed all 29 cases. The original
Python module was then retired; the historical Mull docstring mention remains
provenance. Post-retirement tests passed 13 + 9 + 7 under the isolated native56
extension (`/tmp/forge-generated-engine-post-retirement-tests.log`), and
scoped Clippy passed with `-D warnings`
(`/tmp/forge-generated-engine-post-retirement-clippy.log`).
