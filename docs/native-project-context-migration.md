# Project context native boundary

`conductor.project_context` keeps `ProjectContext`, `Provenance`, `ErrorDetail`,
`ContextError`, `resolve_project_context`, and `require_git_context` as its public
Python API. `_bounded_git`, `_git_paths`, `_read_config`, and `_read_config_bytes`
remain patchable test seams. Python owns path argument type checks and
construction of public dataclasses. Rust owns the
fixed `/usr/bin/git` child process, exact clean environment, new session,
3-second deadline, 32-KiB stdout and 4-KiB stderr caps, and process-group
kill and reap on failure. `_bounded_git` is a thin patchable Python wrapper
for the malformed-topology contract.

`conductor_native.project_context_native(operation, payload_json)` makes the
selection, topology response, containment, ancestry, identity, derived-path,
configuration read-consistency, note grant, and ordered provenance decisions.
Expected refusals return `{ "error": { "code", "field", "message" } }`, which
Python converts to the same `ContextError` detail. Successful operations
return `{ "value": ... }`. Paths cross this JSON seam as hexadecimal raw
filesystem bytes. This preserves Unix names that use Python's surrogate escape
decoding without passing invalid Unicode through JSON. Rust's `evaluate` and
`parse_config` functions also work without a Python interpreter.

The `read-config-bytes` operation performs the complete descriptor-based config
read in Rust. It records metadata before opening, walks from the project root
with `O_DIRECTORY | O_NOFOLLOW` directory descriptors, opens the final file with
`O_NONBLOCK | O_NOFOLLOW`, verifies that the opened descriptor is regular, reads
at most 65,537 bytes, and records metadata again after closing it. It returns
the raw bytes and three `(device, inode, size, mtime_ns)` signatures. The
existing `config-read` decision retains the 65,536-byte limit and rejects a
changed signature. The read operation receives raw path bytes as hex, so Unix
filenames with invalid UTF-8 still work. I/O refusals retain `ContextError`
codes and messages at the Python boundary.

The strict TOML schema parser was already native. Its parser is now a pure Rust
function, with the original PyO3 export translating its schema and parse errors
for callers. The descriptor-based config read remains in Python with its FIFO
and ancestor-swap tests. No path is created by these native operations.

| Behavior | Native test | Existing Python contract |
| --- | --- | --- |
| Repository and linked-worktree keys | `identity_keys_bind_common_git_and_top_without_writing` | `git_identity_is_immutable_has_complete_provenance_and_does_not_write`; `unrelated_repositories_and_linked_worktrees_have_distinct_keys` |
| Project argument and environment priority | `project_selection_preserves_argument_environment_default_priority` | `explicit_project_environment_and_read_only_refusals` |
| Trusted Git process and response forms | `child_receives_exact_environment`; `output_cap_kills_and_reaps_the_child`; `deadline_kills_and_reaps_the_child`; `git_probe_distinguishes_unsupported_and_malformed_responses`; `git_topology_requires_three_absolute_existing_directories` | `relative_git_topology_is_refused`; submodule and separate-Git-directory contract |
| Component-wise containment | `path_membership_uses_components_not_prefixes`; `external_notes_need_an_exact_caller_grant` | `config_schema_paths_external_notes_and_digest`; symlink escape contract |
| Config and derived path safety | `configuration_ancestry_refuses_dangling_symlink_and_non_directory`; `derived_layout_and_existing_ancestor_refusal_are_read_only`; `config_read_rejects_oversize_before_changed_signature` | `config_symlink_escape_and_missing_targets_are_refused`; `config_open_refuses_fifo_without_blocking` |
| Descriptor read and race refusal | `project_context_fs` integration tests for raw bytes, size cap, containment, links, FIFO, and non-UTF-8 names; unit tests for FIFO replacement, ancestor swap, and changed signatures after the first metadata read | `config_open_refuses_fifo_without_blocking` keeps the public resolver check |
| Strict schema | `valid_config_yields_normalized_fields`; `schema_and_parse_errors_stay_distinct` | `config_schema_paths_external_notes_and_digest` |

The Python contract tests remain because they verify the public dataclasses,
provenance order, no-write behavior, and resolver error shape. Direct Python
process mocks were retired after native tests covered the exact environment,
both output caps, deadline, kill, and reap. The former `os.open` race injection
was replaced with deterministic native tests at the exact point between the
first metadata read and descriptor open.

Focused validation passed on 2026-09-27: 17 project-context library tests,
five descriptor filesystem integration tests, and eight PyO3 public contracts
after rebuilding the extension. Scoped Clippy passed with warnings denied for
the library and both context test targets.

Validation for this cohort: 14 native unit tests and 8 Python-contract tests
passed with the rebuilt extension. The native tests include real child
processes for stdout overflow, stderr overflow, timeout, kill, and reap.
