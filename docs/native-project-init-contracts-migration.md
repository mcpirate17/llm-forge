# Native project initialization contracts

The 38 executable cases from `src/conductor/test_project_init.py` are now
Rust-owned PyO3 contracts. They continue calling the production Python APIs;
assertions and reusable test behavior live in Rust.

| Rust target suffix | Preserved cases | Added cases |
| --- | ---: | ---: |
| `project_init_merge` | 12 | 0 |
| `project_init_binary` | 8 | 4 |
| `project_init_run` | 12 | 0 |
| `project_init_manifest` | 6 | 0 |

The resolver itself now lives in `native/conductor-native/src/forge_binary.rs`.
It chooses an explicit nonempty `FORGE_BIN`, then the selected interpreter's
sibling executable, then the project's `.tools/bin/forge`, then PATH. The
interpreter path stays lexical: resolving a venv Python symlink would search
beside the system interpreter and miss the installed venv CLI. An invalid
explicit selection fails loudly. The Python API is a thin native adapter.

SessionStart uses the same search order while retaining its existing empty
`FORGE_BIN` opt-out. Project initialization treats empty as unset; these are
distinct pre-existing caller conventions. The new resolver cases cover venv
precedence, lexical symlinks, explicit selection with invalid-selection refusal,
and non-executable venv fallback.

The original 38 Python cases passed before retirement. An independent review
mapped all 38 to named Rust cases and checked fixtures, exception behavior,
argument shapes, and strict Boolean identity. All 42 Rust cases and the 24
contract-discovery cases passed after retirement. Scoped Clippy, Ruff for the
thin adapters, and shell syntax validation also passed. Provider and helper
changes select the four targets through `python_contract_targets.tsv`.

The isolated installation then passed 117 cases across these targets and the
affected bootstrap, provisioning, hook, read-budget, and cost contracts. That
check uses the installed venv on PATH, as `uv run` does, so subprocess launchers
inherit the interpreter containing the installed native extensions.

Rust test source share remains a separate metric from native production source.
Moving assertions to Rust does not establish a production runtime speedup.
