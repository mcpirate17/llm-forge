# Native hook project seam contracts

`python_contracts_hook_project_seam` replaces the 14 cases formerly in
`src/tooling/hooks/claude/test_hook_project_seam.py` and adds a fifteenth case
for selecting Forge beside the configured interpreter before searching PATH.

The Rust target preserves context merge and CLI behavior, generic-only startup,
project extension output and failure handling, executable selection, selective
pruning, and Obsidian path resolution. It runs the production shell hook,
identity helper, context appender, and pruning helper in temporary host trees.
Rust owns assertions, host construction, and the external-command fixture.
The fixture implements only the preamble, state refresh, telemetry, and gate
protocols needed by these cases; it never launches a model or touches a host's
active state. The production Python APIs remain compatibility test subjects.

The former PATH test depended on the interpreter running pytest having no
adjacent Forge binary. Installation now supplies that binary. The replacement
explicitly selects an interpreter without a sibling for the PATH fallback and
separately checks sibling precedence, including the invoked executable path.

The child executable is declared under Cargo's `python-compat-tests` feature.
`make install` compiles it with the other compatibility binaries and integration
targets. Runtime test execution therefore needs no fixture compilation.

The installed-environment Python baseline reproduced 13 passes and the known
PATH-assumption failure. The replacement passed all 15 cases and scoped Clippy
with warnings denied. Independent review verifies case parity before retirement;
the discovery registry maps both Rust helpers, the child source, and the tested
production files. Nonempty agent identity resolution was outside the original
suite and remains outside this migration's coverage claim.
