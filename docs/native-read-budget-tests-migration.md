# Legacy read-budget tests in Rust

`python_contracts_read_budget` replaces both cases in
`tooling/hooks/agent/test_read_budget.py` with Rust-owned PyO3 and subprocess
assertions. The original two-case pytest baseline, both Rust cases, and scoped
Clippy with warnings denied passed. An independent parity and consumer audit
found no missing assertions or active importers of the retired test module.

`python_binding_counts_and_tallies_in_native_code` preserves the nested JSON
character count of six and exact Python tuple results `(0, 5)` then `(5, 12)`
for successive tallies. `legacy_cli_emits_native_advice` invokes the production
legacy script with the current interpreter, the same 400-character payload,
isolated state directory and 50-token threshold. It requires process success
and checks the parsed JSON additional context for `READ BUDGET`.

The tests call production entrypoints and never import the retired test file.
Historical campaign names remain as provenance. Cargo validation uses two
build jobs, one test thread, and masked GPU access after resource inspection.
