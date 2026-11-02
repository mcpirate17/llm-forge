# Native candidate analysis

`forge candidate analyze` evaluates five bounded source checks against a captured Git candidate:

| `--check` ID | Scope |
| --- | --- |
| `config-parse` | Captured configuration and notebook inputs |
| `python-ast` | Python source structure and changed-line rules |
| `import-declaration` | Python imports against captured manifests and an operator-supplied provider observation |
| `duplicate-function-bodies` | Changed functions compared with eligible captured peers |
| `structure-audit` | Package and tooling structure across captured Python peers |

Example: `forge candidate analyze --check python-ast --repo SNAPSHOT --base BASE_OID --tree CANDIDATE_TREE_OID --execute -- src/conductor/example.py`. The caller supplies explicit `GIT_DIR`, `GIT_WORK_TREE`, and `GIT_INDEX_FILE` bindings to the private Git objects and index; the snapshot directory itself need not contain a `.git` object store. The native candidate execution Stage prepares these bindings. `--base` accepts a full commit OID or tree OID; `--tree` accepts a tree OID. The candidate policy is read from the captured tree (or the explicit `--policy` path within it), classified against that candidate, and its bytes must match the classified policy digest. There is no installed-policy or live-worktree fallback. Selected paths must be captured changes; deleted and ineligible files are reported as excluded.

Without `--execute`, the command emits a plan with `executed:false` and no analysis result. `--execute` adds native findings and metrics. `--tool-version` needs no Git context or files and reports the native analyzer version for the selected check. Exit status is `0` for no findings, `1` when findings are present, and `2` when required inputs or evaluation are incomplete. Check-specific source/configuration parse problems are findings (for example, Python syntax errors are critical findings and malformed configuration is a `malformed-config` finding); invalid UTF-8, resource-limit failures, captured-input reread drift, or malformed/unreadable required peers or manifests produce incomplete status instead.

## Input and resource bounds

Selected changes are limited to 1–4096 unique safe relative paths, each at most 4096 bytes and 128 components. Captured Git inputs must be regular blobs of at most 1 MiB each; the aggregate captured text and serialized analysis payload are each bounded at 32 MiB. The capture inventory is limited to 4096 files. Import analysis inventories at most 4096 captured `pyproject.toml` manifests. Source and manifest bytes are held and rechecked against their captured Git identity before and after evaluation. Duplicate-function and structure checks can require unchanged peer files; unreadable, malformed, or over-limit required peers make the result incomplete.

For checks that use changed lines, Git diff intervals count LF-delimited records. A changed Git record is projected onto every Python physical line it contains, including CR-only and CRLF source line endings. Projection is bounded to 65,536 Python line numbers; invalid or out-of-source Git intervals fail incomplete. This maps candidate Git records to parser line numbering; it does not normalize or rewrite candidate source.

## Import-provider observation

`import-declaration` requires an explicit `FORGE_IMPORT_PROVIDERS` JSON value whenever eligible source files are analyzed. The value has exactly two fields:

```json
{"path":"/absolute/canonical/operator/providers.json","sha256":"<64 lowercase hex SHA-256>"}
```

The observation file must be outside the source and scratch roots, its immediate parent must be owned by UID 0 or the current UID and not group/other-writable, and its bytes must match the supplied digest. The direct CLI excludes the snapshot and `TMPDIR`; Stage additionally excludes the original source, common Git directory, and scratch parent. Its JSON object has exactly these fields: `schema_version` (`1`), `implementation`, `version`, `platform`, `stdlib`, and `modules`. `stdlib` is a list of top-level module names; `modules` is a list of `{ "name": "...", "distributions": ["..."] }` records. Unknown or missing fields are rejected. The report carries the observation path and digest as provenance. This is an operator-pinned environment observation, not proof of the runtime interpreter, installed provider behavior, or an attested package inventory.

## Current boundary

The native Stage mapper binds the base and candidate trees, policy path/digest, and included/excluded file partition to the parent's classified selection. It preserves finding IDs, severity, location and evidence; `metrics.analysis` contains the plan and remaining native result metadata. Incomplete, truncated, malformed or mismatched output becomes a critical analyzer failure. This is execution evidence for the selected check, not full-review completeness, approval or runtime-provider attestation.

Stage capture has the policy's explicit output limit (at most 262,144 characters per stream). The structured response must fit completely; otherwise the attempt is incomplete. Literal Unicode replacement characters are conservatively refused by the Stage mapper because its normalized output tail cannot distinguish them from lossy decoding. The direct CLI's result bound is 32 MiB.

Focused release validation passed 173 native candidate unit tests, 21 AST/behavior contracts, 27 execution-result tests, seven policy-loader tests, 17 analysis CLI cases and the provider-location admission test. Eight explicitly enabled tests passed under Landlock/seccomp and delegated cgroups, checking all five check IDs, provider read grants/hash drift, resource refusal, original source preservation and owned cleanup. Separately, 44 targeted compatibility tests passed against a freshly built binding in an isolated import directory, covering remaining review callers, standalone duplicate fingerprints, baselines and Git selection. The committed-tree native gate is separate and excludes these optional bindings.

The [current-only evaluator benchmark](../research/performance/native-candidate-analysis-20261101.json) records warm medians of 0.572 microseconds for one Python statement, 1.413 for one TOML file, 4.526 for one import file, and 11.239 for a duplicate candidate plus peer. It includes parsing and result allocation/drop but excludes JSON decoding, Git/file capture, process and sandbox costs. It establishes neither end-to-end dispatch latency nor a speedup against a baseline.
