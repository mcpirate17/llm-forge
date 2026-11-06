# Native complexity report and ratchet

`forge complexity` implements the pinned `radon-6.0.1-cpython-3.12` block
projection without importing Radon or discovering an interpreter. It reuses
Forge's source-bound Ruff admission; unsupported grammar and exceeded resource
limits fail explicitly. This is source complexity analysis, not execution or
approval evidence. The protocol identifies metric semantics; installed binary
identity remains a separate operator/grant concern.

```sh
forge complexity --tool-version
forge complexity report --path src
forge complexity check --path src
forge complexity refresh-baseline --path src
```

Host root resolution and `CONDUCTOR_HOST_ROOT` use native project_paths.
`--repo` supplies an absolute host start directory when needed. Baseline
precedence is explicit `--baseline`, nonempty `CONDUCTOR_RADON_BASELINE`,
authoritative `forge.toml` `[conductor].radon_complexity_baseline`, then legacy
`pyproject.toml` `[tool.conductor]`, then
`conductor/radon_complexity_baseline.json` under the host. A present forge.toml
does not merge missing fields from pyproject.toml. Explicit baseline flags may
be absolute or relative to the host; configured baseline paths remain
root-relative. Other configured project paths retain existing validation.

Default scopes remain `conductor`, `research`, `aria_core`, `aria_designer`;
they are not inferred from the installed package or package_root. Repeat
`--path` to select other scopes, including explicit external paths or files
without a .py suffix. Missing scopes are empty. Directory scans include hidden
*.py names, sort paths, and do not recurse through child directory symlinks.
Explicit scopes resolve symlinks; file symlinks read their bounded regular
resolved target. Repeated scopes remain repeated observations.

Default exclusions remain `*/rust/*`, `*/tests/*`, `*/tmp/*`,
`*/research/tmp/*`, `*__pycache__*`. Supplying any `--exclude` replaces that
list; patterns use the existing native fnmatch implementation, with stars
matching path separators. `--min-rank` accepts A–F, default D. Report details
are capped at 50 rows while totals and baseline rows cover the admitted scan.

Checks compare source-generated path::name (or path::Class.method) keys and
scores, retaining the lowest baseline score for duplicate keys. New or worsened
flagged blocks fail; improved grandfathered blocks are reported even below the
current threshold. Metadata fields in the baseline do not alter source scope,
exclusions, threshold or ratchet identity. Refresh writes flagged rows plus
selected options as sorted-key, indented ASCII JSON with a final newline; it
never silently tightens the baseline during check.

Report/refresh succeed with exit 0; check uses 0/1 for held/failed ratchets.
Source read, syntax, unsupported grammar and scan admission errors use exit 2
before any baseline write. Baseline/config/path resolution, malformed/missing
baseline and publication failures use exit 1. Invalid CLI syntax uses exit 2.
Report resolves the baseline path but does not require its file to exist.

Admission is 1 MiB/source, 32 MiB aggregate source work (including repeats),
4096 candidate source occurrences, 65536 traversal entries, depth 128, 65536
aggregate blocks, 4096-byte paths, 128 exclusion patterns and 128 MiB of
conservative fnmatch work. Baseline input/output and diagnostic work are capped
at 16 MiB. Core parser and per-file block limits remain in force. Exceeded
limits refuse the whole operation. FIFO/device inputs cannot block the reader.
Malformed baseline keys/scores must be strings/i64 integers; Python's former
str()/int() coercion of malformed rows is deliberately refused. Root-list and
scalar baselines remain unsupported. Refresh reuses owned, singly linked,
regular-file atomic publication under an existing parent, rather than partial
truncation or writing through special files; new files are private mode 0600.
The parent and any existing baseline must belong to the current user and deny
group/world writes, under the [shared publisher contract](native-candidate-review.md).

`complexity-report`, `complexity-check`, and `complexity-refresh-baseline` in
conductor.mk call this route; `COMPLEXITY_ARGS` supplies host-specific options.
`baseline-complexity` scans src and obtains its baseline through project config.
The candidate policy keeps the check in both fast and full profiles and uses
native Forge for execution/version reporting. Shared baseline merge support
and its tests remain unchanged.

## Reference validation

The native release CLI agreed with CPython 3.12.3/Radon 6.0.1 on 93 frozen
comparisons: 89 exact block projections and four matching syntax refusals.
This includes all 61 upstream source snippets, 25 authored edge sources, and
seven real repository files containing 168 blocks. All reported block fields
and stable output ordering were compared. The comparison exposed a leading
UTF-8 BOM difference, corrected in the strict compatibility parser; the general
source parser is unchanged.

The Rust suite also consumes the 61 upstream literal expectations and an
independently captured 50-variant edge oracle, covering raw block ordering,
module complexity and both assertion options without an interpreter at test
time. See [fixture provenance](../native/conductor-native/tests/fixtures/radon_complexity/PROVENANCE.md).
Thirty installed CLI tests cover the ratchet, caller options, path/config
precedence, atomic publication and input bounds. The shipped Make check also
passed over this repository's 1,519 remaining Python blocks against its existing
baseline. These are scoped comparisons, not exhaustive Python grammar parity.
