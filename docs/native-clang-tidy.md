# Native candidate clang-tidy

`forge candidate clang-tidy --repo SNAPSHOT --base BASE_OID --tree TREE_OID
--execute -- FILE...` analyzes captured host C/C++ sources and headers. Omit
`--execute` for a bound plan. `--tool-version` probes an explicitly provisioned
native ELF without opening Git. The policy supplies `--repo {snapshot}`; `{repo}`
denotes the original checkout and is unsuitable for this worker.

Use the existing candidate execution Stage with explicit tool and read-only
input grants. Direct invocation creates no sandbox. The Stage supplies private
Git metadata, the captured snapshot, scratch, cgroups, filesystem/network policy,
timeouts and descendant cleanup. There is no Git subprocess or Python wrapper.

## Operator context

`FORGE_CLANG_TIDY` names a canonical absolute ELF owned by the effective user or
root, without group/other write permission, outside the snapshot and scratch.
There is no PATH lookup, installer or fallback. Executable identity is rechecked.
The policy pins the normalized version probe to `LLVM version 22.1.8` and disables
result caching. The operator provisions the tool; `uv.lock` does not pin it.

`FORGE_CLANG_TIDY_CONTEXT` names a canonical external JSON file;
`FORGE_CLANG_TIDY_CONTEXT_SHA256` supplies its exact lowercase SHA-256. Duplicate
and unknown JSON fields are rejected. A minimal context is:

```json
{
  "schema_version": 1,
  "source_root": "/original/project",
  "database": {"kind": "snapshot", "path": "compile_commands.json"},
  "resource_dir": "/operator/clang/lib/clang/22",
  "mappings": [],
  "header_units": {"include/api.h": ["src/unit.cpp"]}
}
```

`source_root` is the logical original root in the database, mapped to the current
snapshot. Optional mappings have `original` and `admitted` absolute directory
paths. They can redirect original build directories or SDK roots to explicitly
admitted external directories. Paths are resolved lexically before rebasing;
unknown paths and flags fail rather than being guessed.

Artifacts are either `{"kind":"snapshot","path":"relative/file"}` or
`{"kind":"external","path":"/operator/file","sha256":"<64 hex digits>"}`.
An external database additionally requires `source_tree` equal to the exact base
or candidate tree OID. This is the operator's applicability assertion, not proof
that a build generated it. Optional `config` uses the same artifact forms;
otherwise the nearest captured `.clang-tidy` above the translation unit is used,
with explicit defaults if none exists. Config inheritance, aliases, tags,
duplicate keys and compiler-argument override keys are unsupported. Scalar
option text is preserved when YAML is converted to JSON.

Source/config files are checked against Git blobs; external files are checked
against their hashes. Files must be regular and singly linked. Directories are
identity-bound, not recursively hashed. Operator-owned ancestors, SDK/resource
contents and object storage must remain stable. These checks do not attest
against hostile same-UID races or capture every transitive include. Existing
Stage read grants still govern compiler access, including original object storage.

## Analysis contract

Every selected translation unit's database configuration is retained and runs
separately. Both `arguments` and `command`, if present, must agree. GNU command
quoting follows LLVM 22's Linux database dialect with stricter malformed/control
input refusal; nothing is evaluated by a shell. Known C/C++ driver basenames
select a virtual Clang frontend identity, never execution of database `argv[0]`.
This is Clang analysis, not compiler equivalence with an original GCC toolchain.

Typed include/sysroot paths are rebased. Output-only flags are removed; response
files, plugins, PCH/modules, arbitrary frontend options and unknown flags are
refused. Forced include files must be captured source inputs. Generated include
directories need explicit external mappings. CUDA/HIP translation units are not
admitted by this host C/C++ tool.

Deleted files and pure deletions do not expand to whole-file checks. Changed
headers need database entries or explicit `header_units` mappings. Successful
mapped-header checks must observe the required headers in that configuration's
`-H` trace. The line filter uses full snapshot paths and inclusive changed ranges.
If Clang reports an alias such as `left/../include/a.h`, one bounded retry adds
that exact spelling with the same ranges. Changed spellings on retry fail.
This is observed compiler output, not authenticated dependency evidence.

The worker forces warnings-as-errors, header diagnostics (including system
headers), an empty excluded-header filter, no fixes, no formatting and no color.
Configured checks and source suppression directives otherwise retain LLVM
semantics; success does not prove every physical line received every check.
Compiler errors and tool failures remain failures. Missing context, unsupported
inputs, incomplete coverage, drift and cleanup errors return exit 2 with an
explicit `clang-tidy incomplete` diagnostic. The parent Stage records these as
failed checks; it does not yet expose a separate incomplete result enum.

## Bounds and measurements

Admission caps include 32 MiB raw/decoded database data, 100,000 records,
4,096 arguments per record, 64 KiB tokens, 1 MiB command strings, 1,000,000 total
arguments, 4,096-byte paths, 4,096 selected configurations, 8 MiB per captured
source and 32 MiB aggregate source inputs. Context/configs are 64 KiB each;
aggregate configs are 1 MiB. Normalized database/config output is capped at
64 MiB, line filters at 64 KiB, and invocation argv at 128 KiB.

Each analysis attempt uses the existing native process monitor with a 120-second
and 1-MiB combined-output bound inside the enclosing Stage's budget. Header alias
handling permits at most two attempts per configuration. The version probe has
a 30-second/16-KiB bound. Children inherit only `PATH=''`, `LANG=C`, and private
HOME/TMPDIR. Unique mode-0700 runtime directories contain mode-0600 generated
databases/configs and are removed explicitly; cleanup failures identify residue.

The current-only release API benchmark measured a 1,000-record database at
995.748 microseconds median / 1008.656 microseconds p95, and a 17-argument rebase
at 2.679 / 2.801 microseconds. Five warmups preceded 31 samples of 20 calls,
including result destruction. CLI startup, filesystem planning, process launch,
LLVM analysis and sandboxing are excluded; this establishes no baseline speedup
or end-to-end dispatch claim. [Raw results and source hashes](../research/performance/native-clang-database-20261101.json)
record the unisolated Ryzen 9 9950X3D host and Rust 1.98.0 toolchain.

Reproduce with `cargo +1.98.0 bench --offline --locked --manifest-path
native/conductor-native/Cargo.toml --bench candidate_clang`. Real LLVM and Stage
tests are conditional on the explicit tool/resource paths and delegated cgroup;
default native tests alone do not establish those integrations.

Focused release validation on 2026-11-01 passed 68 cases across the database,
rebasing, lexer/hook, bounded-reader, Clang unit and CLI suites. This includes
four real delegated-stage regressions and a real LLVM config-dump comparison.
The Stage cases verify dirty-source preservation, changed-line controls, all
database configurations, header spelling/missing inclusion, system-header
diagnostics, resource enforcement and cleanup. Strict Forge Clippy passed.
The explicitly provisioned clang-tidy 22.1.8 ELF SHA-256 was
`d77c4ac9c08b71a1e2b9a2950f91d2b398754237a23b3f60acb4601033461483`.
