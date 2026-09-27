# Mutation campaign and receipt contracts

The 21 named cases in `src/conductor/test_mutation_testing.py` and six in
`src/conductor/test_mutation_receipt_encoding.py` have Rust-owned PyO3
replacements: `python_contracts_mutation_testing_core.rs` (14 named),
`python_contracts_mutation_testing_evidence.rs` (seven named), and
`python_contracts_mutation_receipt_encoding.rs` (six named). The original
two-suite CPU baseline passed 44/44 expanded pytest executions. Rust loops
preserve all ten complete-scope, six legacy-anchor, and four runner-provenance
parameter rows. The exact Rust targets passed 14/14, 7/7, and 6/6, and scoped
Clippy passed with `-D warnings`. Independent case-by-case review passed after
the inspection callbacks were made to reject keywords and return fresh lists,
as the original `lambda *_args` fixtures did. The two original Python modules
were then retired together. Post-retirement exact Rust cases passed again
(`/tmp/forge-mutation-testing-receipt-postretire-tests.log`), as did scoped
Clippy (`/tmp/forge-mutation-testing-receipt-postretire-clippy.log`).

`tests/python_contracts/mutation_testing_fixture.rs` constructs the original
temporary `Campaign`, ranked tests, planned mutations, test-scope tuples,
registry, and synthetic PASS receipt in Rust. The receipt-encoding Python test
used to import three private helpers from the campaign Python test; the Rust
targets share this Rust fixture instead, allowing the two originals to retire
together. Test callbacks and assertions also live in Rust; the contracts call
existing Python production APIs through PyO3.

The core target checks contiguous ranks, manifest and patch refusals,
complete/partial Python scope inventory, process evidence supplied as text,
host-read file materialization, and interpreter pinning. The evidence target
checks native PASS verification, non-UTF-8 receipts, exact local Git anchor
bytes and paths, all six legacy-anchor failures, all four runner-component
drift rows, scope rejection, and unregistered tests. Receipt encoding checks
shared nodeid-table interning, unchanged legacy expansion, malformed-index
refusal, unranked values, and acceptance by the native evidence verifier.

The checked-in campaign fixture reads and hashes
`src/conductor/testdata/mutation_testing/patches/pack_mode_first_order.patch`,
along with its campaign JSON and toy source/scope files. These are existing
fixture bytes and remain unchanged. The tests create only temporary Git
repositories and synthetic receipt data; they never apply the patch, run a
mutation engine, or create a worktree. The separate 21-case
`test_mutation_testing_support.py` suite exercises real process groups and is
left for a dedicated bounded cohort.

Historical nodeids in `mutation_runner_lineage.json`, the candidate-review
grandfathered inventory, fixture titles/docstrings, and graph-context string
assertions remain provenance/data, not live Python test-module imports.
