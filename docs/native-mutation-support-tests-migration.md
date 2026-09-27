# Mutation command support contracts

`src/conductor/test_mutation_testing_support.py` contained 21 named Python tests.
The isolated baseline passed 21/21 (`/tmp/forge-mutation-support-baseline.log`).
Its two Rust-owned PyO3 targets preserve those named cases:

| Target | Cases | Behavior |
| --- | ---: | --- |
| `python_contracts_mutation_support_host` | 8 | Host-read merge, JSON publication, registry paths and entries |
| `python_contracts_mutation_support_process` | 13 | Command groups, timeout and clean-exit reaping, PDEATHSIG, dry-run and apply |

The Rust process fixture owns every child or process group it starts. It kills
only recorded fixture groups and waits for its children on cleanup, including
when an assertion fails. Its child binary has three bounded modes: call the
existing `run_command` API as an engine, create a group whose leader exits, and
read the Linux PDEATHSIG setting. The tests do not run mutation engines,
create Git worktrees, or contact services. The original 2-second parent-death,
10-second reap and 30-second command bounds remain assertions. They preserve
the four host merge outcomes and exact registry, error, path, and output checks.

Provider selection must include `mutation_testing_support.py` for both targets;
`mutation_testing.py`, `project_paths.py`, and the checked-in
`testdata/mutation_testing/campaign.json` fixture for the host target; and the
target-specific Rust helper, child fixture, and common test helpers. Historical
references to the Python node IDs in `mutation_runner_lineage.json` and the
coverage fixture docstring remain provenance data and are not live imports.

Independent parity and consumer review passed
(`/tmp/forge-mutation-support-independent-review.md`), then the Python
original was retired. Post-retirement validation under the isolated native55
extension passed all 8 + 13 Rust cases
(`/tmp/forge-mutation-support-post-retirement-tests.log`) and scoped child plus
target Clippy with `-D warnings`
(`/tmp/forge-mutation-support-post-retirement-clippy.log`). The historical
lineage and coverage references remain unchanged as provenance.
