# Native candidate clang-format

`forge candidate clang-format --base BASE_OID --tree TREE_OID -- FILE...`
prints a bound changed-line plan. Add `--execute` to run formatting checks.
`--tool-version` probes an explicitly provisioned native formatter without
opening Git. The command uses Rust and libgit2; it does not execute Git or Python.

The working directory (or `--repo`) must be a canonical candidate snapshot with
the explicit `GIT_DIR`, `GIT_WORK_TREE`, and `GIT_INDEX_FILE` supplied by the
native candidate execution stage. The private index must exactly match the
expected tree, and private HEAD must identify that tree. The base must be a full
commit or tree object ID, including the empty-tree ID for an initial candidate.
No ambient ref discovery occurs. The constructor also understands the older
symbolic private HEAD layout; this is not a claim that the remaining Python
review runner supplies native tool grants.

## Scope and refusal

Line ranges come from immutable base/candidate blobs and verified change
metadata. Rename and copy sources use their recorded old paths. Ranges are
one-based, inclusive, sorted and coalesced. LLVM can format surrounding syntax when given a line
range; this wrapper does not promise that only those physical lines are checked.
Deleted files, pure line deletions and changes without new-side text have
explicit non-executing scopes; an empty
selection never expands to a whole-file check. Binary, non-UTF-8, nonregular,
missing, mismatched or over-budget inputs fail explicitly.

The CLI accepts up to 4,096 selected paths. The shared range API admits 100,000
paths, 8 MiB per blob, 32 MiB aggregate source bytes, 100,000 lines per diff side,
100,000 hunks/ranges and 16 MiB encoded plan output. Tree lookup, metadata,
path-byte and weighted comparison-work budgets also apply. The conservative
diff-work bound can refuse a large file even when its actual diff is small.
These are admission limits, not a hard CPU or I/O deadline.

Before and after execution, descriptor-confined reads verify selected snapshot
bytes against captured Git blob IDs. Private index/HEAD/root identity is checked
at batch boundaries. Operator-owned ancestors and object storage must remain
stable; these checks detect ordinary drift, not malicious same-UID races.

## Styles and executable provisioning

The nearest captured `.clang-format` or `_clang-format` is passed explicitly to
the tool. With no captured style, the command selects LLVM explicitly, so an
ambient host style is not consulted. Style reads are capped at 64 KiB each and
1 MiB total. Native YAML event validation bounds nesting/events and rejects
aliases, anchors, tags and decoded `InheritParentConfig` scalars. Relevant ignore
files and ambient ancestor ignore files are unsupported and cause refusal.
These restrictions prevent an unaccounted parent style or ignore file from
changing selected checks. Candidate style settings and source directives such
as `DisableFormat` and `clang-format off` otherwise retain LLVM semantics; a
successful formatter result does not prove every physical line was formatted.

`FORGE_CLANG_FORMAT` must name a canonical absolute native ELF executable owned
by the effective user or root, without group/other write permission, outside
the candidate and stage scratch. No PATH lookup, interpreter wrapper, installer
or implicit download is used. Each child gets an empty inherited environment
with only `PATH=''` and `LANG=C`. Executable identity is rechecked. The version
probe has a 30-second/16-KiB bound; actual formatter processes use the enclosing
stage's timeout, cgroup, filesystem grants and descendant cleanup.

The `clang-format` policy check invokes the granted Forge alias with
`--base {base} --tree {tree} --execute -- {files}` and has caching disabled.
Operators grant both the installed Forge executable and the formatter path,
including its dynamic-library read dependencies. Direct `--execute` invocation
does not itself create a sandbox; use `forge candidate execute-batch` for
supervised product execution. Each selected file runs sequentially and the
latest nonzero tool exit is retained. Spawn, signal, config and drift errors
fail explicitly rather than being reported as clean skips.

The Python `clang_files.py` wrapper is deleted. Its tidy path is replaced by
[native clang-tidy](native-clang-tidy.md); shared Python diff-range consumers
remain until their own migrations finish.
The policy's previous claim that `uv.lock` pinned Clang was false. Validation
uses an explicitly provisioned native clang-format 23.1.0 artifact; it does not
install or import its Python wheel wrappers.

## Verification

Release validation on 2026-11-01 passed 122 candidate unit tests, two confined
reader tests, two private-context CLI tests and two real delegated-stage tests.
Strict Clippy passed. The real tests used native clang-format 23.1.0, verified
resource enforcement/cleanup, preserved dirty originals, passed changed-line
checks despite unrelated old formatting drift, and caught new formatting drift.
Those two cases require `FORGE_TEST_CLANG_FORMAT` and
`FORGE_TEST_CGROUP_PARENT`; the default suite skips them. The formatter ELF had
SHA-256 `2c143542f65a0553e1805a6699a4abfa415b0324982eb2d438daf6bd2ede8897`.

The current-only synthetic range benchmark used 16 C files of 100 lines each,
with line 51 changed in each file. Five warmups preceded 31 samples of 20 calls.
Median sample mean was 581.588 microseconds and p95 was 590.852 microseconds for
the complete 16-file API call including result destruction. The unisolated
Ryzen 9 9950X3D host ran no concurrent owned build during the timed region.
Fixture creation, candidate resolution, CLI startup, sandboxing and actual LLVM
execution are excluded. These measurements establish neither speedup nor
end-to-end dispatch latency. [Raw results and hashes](../research/performance/native-clang-lines-20261101.json)
bind the source, benchmark executable, fixture and expected output.

Reproduce with `cargo +1.98.0 bench --offline --locked --manifest-path
native/forge/Cargo.toml --bench candidate_lines`.
