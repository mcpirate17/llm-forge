# Mutation testing in llm-forge

How campaigns are generated, run and audited. `forge mutation plan|write` is the
native campaign writer and `forge mutation inspect|run` the native runner; hosts that
have not migrated keep `conductor.mutation_campaign_generate` and
`conductor.mutation_engine_generated`. Native corpus auditing is available through
`forge candidate mutation-corpus`; its legacy caller remains
`conductor.mutation_patch_audit`. Other compatibility commands include `conductor.complete_scope_drift`
(local check for `complete`-mode scope drift before the audit catches it) and
`conductor.mutation_retention` (sweep); `make mutation-*` wraps the common flows.
Forge ships these capabilities without requiring mutation campaigns or receipts for
its own changes. A host such as LLM can adopt a separate evidence policy.

## Installed native runner identity

On Linux, `forge mutation identity` prints a bounded JSON description of the running
executable: compiled Git revision, compiled source fingerprint and measured executable
SHA-256. Dirty or unknown builds are refused. Before evaluating receipts, an operator
can independently select a clean binary, save its description outside the host, Git
common directory and scratch roots in a private directory, then pin those exact bytes:

```sh
umask 077
/selected/forge mutation identity > /operator/private/forge-native.json
sha256sum /operator/private/forge-native.json
```

Pass both `--native-runner-pin /operator/private/forge-native.json` and
`--native-runner-pin-sha256 HEX` to native `candidate mutation-corpus`, `candidate
review`, `candidate gate`, or the candidate benchmark. The schema is
`llm.mutation-testing.native-runner-pin.v1`; unknown fields are refused. Selecting
these independently measured bytes is an explicit operator input. A receipt, project
file, environment variable or model response cannot supply that authority.

Installed mode is eligible only when all four Forge source roots (`native/forge`,
`native/forge-cli`, `native/conductor-native`, `native/slop-core`) are absent. Complete
in-tree inventories retain source-fingerprint validation; partial, symlinked or
unreadable inventories never enable fallback. Inputs and absence observations are
held and rechecked through final disposition. The running binary and all three
receipt identity fields must match the selected pin; dirty receipts still fail.
Live baseline publication cannot overwrite the pin or a hardlink alias.

All-native corpus audits need neither a Python runner bundle nor `uv.lock`.
Candidate review and gate retain their existing engine-integrity requirement for
the candidate's exact `conductor-tooling` Git revision in `uv.lock`, held through
final verification. That candidate-controlled lock is additional consistency data,
not operator authority. Legacy Python receipt validation still needs its separately
selected compatibility bundle; Python APIs do not gain an ambient installed-pin path.

This establishes cooperative identity consistency and freshness, not compiler,
producer or execution attestation. Source/test/manifest hashes, complete measured
mutation coverage, outcome accounting and test-quality requirements still apply.
Identity acceptance alone does not establish corpus cleanliness or candidate approval.

## Optional risk-ranked planning

`forge mutation plan` reports what `forge mutation write` would publish. Without
`--only` it covers the files this branch changed against `--base` (default
`origin/main`), plus dirty files inside this lane's active ownership claims; a dirty
checkout with no claim is refused, as is a branch with nothing changed. `--all-files`
is the explicit whole-tree maintenance sweep. `--verbose` adds the full manifests.
Refusals print `{"status": "REFUSED", "error": ...}` and exit 4. `write` refuses to
replace an existing manifest, whose recorded survivor baseline would be lost, unless
`--force` is given.

The planner can also rank an exact changed-file scope without running an engine or
writing campaigns:

```bash
forge mutation plan rust \
  --priority-policy candidate_policy.toml --priority-base origin/main \
  --only native/forge/src/mutation_write.rs
```

`--priority-policy` must name the host's configured candidate policy, resolved
through `[tool.conductor]` and the existing environment override. Only its
explicit `[risk].high` production/validation paths are eligible. A different
permissive policy cannot widen the host's scope. Missing policy data fails
explicitly; discovery paths remain exempt. Priority requires exact `--only`
paths and a verifiable base commit; it does not support a whole-tree inventory.
`--priority-base` defaults to `--base`, and the Python
`mutation_campaign_generate plan/write` wrapper exposes the same `--priority-policy`
option. Commands without the priority option retain ordinary planner behavior.

Rust source ownership follows declared targets and module reachability when a
package borrows source from another package's directory. CLI-only modules in
Forge's external `forge-cli` target are currently refused explicitly; assigning
them to the neighboring `forge` library would test the wrong package. Ordinary
library scopes remain available. The #184 reproduction is addressed by moving the
planner and lane identity into the shared library:
`forge mutation plan rust --only native/forge/src/mutation_plan.rs --include-covered`
now selects package `forge`. `cargo mutants --list --manifest-path
native/forge/Cargo.toml --package forge --file src/mutation_plan.rs` discovers
38 mutations at this change. Discovery is not execution or mutation PASS evidence.
Other CLI-only modules still require a supported package/source layout.
Without `--include-covered`, an already registered crate is skipped; `READY` with
zero newly generated campaigns is not evidence that its current sources passed.
This is bounded syntactic ownership discovery, not Cargo macro expansion. Unknown
item-generating macros and conditional path transformations are refused rather
than treated as proof of ownership.
Explicit imports that shadow `include!` are refused; glob-imported macro names,
procedural expansion and function-generated modules are outside this syntactic
analysis. Engine discovery and execution remain separate evidence.

Priority produces one campaign per eligible changed source. Cargo `--file`
patterns remain package-relative; source/test hash keys remain repository-relative.
Parsed Cargo target/module reachability excludes proven test-only files, defers
unknown roles, and preserves production files containing inline tests. A changed
test cannot pull an unchanged production file into the authorized scope. Missing
in-scope test/source pairings are visible diagnostics, without inventing a new
mutation obligation for exempt discovery code.

The deterministic order is source novelty, changed paired tests, a matching
caller-supplied impact hint, then source path. New source precedes changed source;
a byte-identical rename gets no novelty bonus. This is an ordinal heuristic,
not a calibrated failure probability. Unknown test identities are reported as
gaps, not counted as demonstrated new tests. Existing manifests never establish
current mutation PASS or coverage: those fields remain unknown in this plan.

`--priority-telemetry PATH` accepts optional **unverified caller-supplied hints**
under `campaigns[CAMPAIGN_ID]`. A hint row must match the emitted `tree_oid`,
`worktree_sha256`, `policy_sha256`, `source_sha256` and `test_sha256` fields.
Optional numeric fields are `cost_hint_seconds` and `dependency_impact_hint`.
Matching public hashes do not authenticate the producer or establish observed
runtime, current graph reachability, binary dependencies, or ignored artifacts.
Missing or mismatched hints remain unknown.

`--priority-budget-seconds N` uses matching supplied cost hints to select campaigns.
Unknown costs and campaigns exceeding the remaining estimated budget are visibly
deferred. This budget is an estimate, not an engine timeout or execution cap.
Every deferred row remains in `priority.ranked`; no engine is launched implicitly.

### Native test admission

When a host enables its mutation-evidence admission check, native-only changes
participate alongside Python tests. Rust test identities come from parsed test
definitions and actual Cargo/module context, including external test modules;
source hashes include those external test files. Attribution must also bind the
Cargo test target. A matching leaf function name or another executable's result
cannot prove a test ran or killed a mutant.

Recognized test-generating macros, ambiguous module/target contexts and unavailable execution
mappings fail explicitly as unsupported evidence. Ordinary production macros
are not compiler-expanded by this parser; accepting an ordinary production macro
does not prove that it cannot generate tests. C/C++ function spelling alone cannot
establish a CTest case: without a verified CTest-to-source mapping, admission remains
unsupported. These checks neither enable a host gate nor launch a campaign.

### Opt-in Cargo per-test value evidence

The opt-in Cargo path binds one explicitly declared integration-test target to an
exact selected production-source set. It runs two to five unmutated baseline
repetitions and uses the automatic cargo-mutants scenarios and their observed test
reports for per-test attribution. The ordinary generated Cargo path remains
available without this opt-in; a generic multi-target run does not establish
per-test value evidence. Neither an authored declaration nor a receipt alone
approves a candidate or authenticates the producer.

Author a repository-relative JSON declaration before planning. For example, a
root package `demo` with `src/lib.rs` declaring `pub mod a; pub mod b;` and one
`[[test]]` target named `contract` at `tests/contract.rs` can use:

```json
{
  "schema_version": 1,
  "target": {
    "manifest_path": "Cargo.toml",
    "package": "demo",
    "test": "contract"
  },
  "baseline_repetitions": 2,
  "required_contracts": [
    {"id": "behavior", "criticality": "high", "active_paths": ["src/a.rs", "src/b.rs"]}
  ],
  "ranked_tests": [
    {"rank": 1, "nodeid": "tests/contract.rs::checks_behavior", "contract": "behavior", "rationale": "checks both selected sources"}
  ]
}
```

This first slice accepts exactly one explicit contract, with criticality `high`
or `critical`, whose unique `active_paths` equal the selected Rust source set.
Ranks are ordered and contiguous from one; node IDs and libtest identities must
be unique, resolve to the declared target's statically discovered tests, and map
to the contract ID. `intentional_redundancy: true` is not supported. The
criticality and rationale are operator/author declarations, not measured facts.

Independently pin the declaration's exact bytes, then plan and write with the
same scope and one worker. Use the actual manifest path printed by `write`:

```sh
DECL_SHA=$(sha256sum contracts/value.json | cut -d ' ' -f 1)
forge mutation plan rust --owner my-lane --base origin/main \
  --only src/a.rs --only src/b.rs --jobs 1 \
  --rust-value-contract contracts/value.json --rust-value-contract-sha256 "$DECL_SHA"
forge mutation write rust --owner my-lane --base origin/main \
  --only src/a.rs --only src/b.rs --jobs 1 \
  --rust-value-contract contracts/value.json --rust-value-contract-sha256 "$DECL_SHA"
# After independently selecting the tools and an existing delegated cgroup:
export RUSTUP_HOME=/absolute/rustup-home
export CARGO="$RUSTUP_HOME/toolchains/TOOLCHAIN/bin/cargo"
export FORGE_MUTATION_CGROUP_PARENT=/sys/fs/cgroup/DELEGATED_PARENT
forge mutation run campaigns/PATH-FROM-WRITE.json --owner my-lane --base origin/main \
  --only src/a.rs --only src/b.rs --allow-mutations --receipt evidence/cargo-value.json
```

The declaration is input to the automatic writer, not a hand-edited campaign or
receipt. The generated test command binds `Cargo.toml`, package and integration
target and uses `--locked --offline`. The declaration and SHA must agree with
independently captured current bytes. Inventory metadata separately binds the
root manifest, lockfile, all package Rust files and the declared test discovery;
it does not widen `source_sha256` or mutation authority beyond selected sources.
Adding a relevant Rust input or a forbidden Cargo configuration refuses stale
metadata. Captured candidate and live inputs are held and rechecked.

The supported first slice admits cargo-mutants 27.1.0 and a dependency-free root
package with root `Cargo.toml` and `Cargo.lock`, an explicit standard-harness `[[test]]` target,
and the supported static library/module and integration-test forms. Workspaces,
nested manifests, `.cargo` configuration, build scripts, symlinks, dynamic
includes, unsupported macros, conditional discovery and ambiguous targets or test
identities are refused. This is bounded syntactic discovery, not compiler or
runtime-data closure. During automatic collection, unrecognized report phases,
missing attribution, red baselines, timeouts, stale inputs or failed restoration
prevent complete per-test evidence. Later receipt validation does not independently
replay those phases or prove their execution.

The opt-in native actor requires `CARGO` to resolve to a direct canonical
`<RUSTUP_HOME>/toolchains/<name>/bin/cargo` executable under the selected
`RUSTUP_HOME`; Forge does not invoke rustup to select it. The operator must
supply an existing delegated cgroup v2 parent, canonical and below `/sys/fs/cgroup`, through
`FORGE_MUTATION_CGROUP_PARENT`; Forge does not enable controllers or create the
parent. It refuses a configured Python guest or campaign environment overrides.
Commands run with the captured source snapshot read-only, private writable runtime
scratch and network denial. The sandbox grants read/execute access to the selected
toolchain and fixed system roots including the broad `/usr` tree (and, when
present, `/lib`, `/lib64` and `/etc/ld.so.cache`); it does not grant arbitrary
home directories or the cgroup filesystem to the child. Verify that these grants
and the delegated cgroup match the host's policy before using the path.

The receipt records normalized report and phase observations, scenario-log
hashes, and tool and cleanup observations. A scenario-log hash binds its captured
whole log; it is not a per-phase byte hash or an authenticated producer claim.
Retained phase metadata and log hashes are not independent execution proof.
The observations do not attest compiler or dynamic library closure or prove that
a later execution used the same environment. If
owned process cleanup is unknown, the actor retains its runtime and snapshot
rather than claiming safe cleanup. Review the receipt's accounting, attribution,
test-value and cleanup results independently of any candidate approval.

## Ordinary Cargo execution grants

On Linux, the ordinary root profile admits a dependency-free Cargo package with
one explicit standard-harness integration target. The explicit workspace profile
below also supports captured local paths and verified registry seeds. Both require
one worker, no campaign environment overrides,
no configured Python guest and operator-pinned execution grants. Ordinary Fest
and Mull execution remain unsupported under these grants (#196). The opt-in
Cargo per-test value workflow above retains its separate execution interface.

Ordinary execution requires an operator-owned, pinned grant document:

```sh
forge mutation run campaigns/PATH-FROM-WRITE.json --owner my-lane --base origin/main \
  --only src/a.rs --allow-mutations \
  --execution-grants /operator/private/grants.json \
  --execution-grants-sha256 SHA256_OF_EXACT_GRANT_BYTES
```

The strict JSON document has `version: 1`, `source_root`, `scratch_parent`,
`cgroup_parent`, `read_only`, `programs`, `resources`, and `max_wall_seconds`;
unknown fields are refused. Paths must be canonical absolute paths. `source_root`
must be the repository root, `scratch_parent` an existing private-root parent,
and `cgroup_parent` an existing delegated cgroup v2 parent. `read_only` lists
explicit files or directories. `programs` maps each alias to an object with
`path` and lowercase `sha256`; ordinary Cargo requires `cargo`, `cargo-mutants`,
`rustc` and `rustdoc`, plus any helpers the toolchain actually needs. `resources`
contains integer `cpu_millis`, `memory_mib` and `processes`; `max_wall_seconds`
is an integer from 1 through 86400. Grant the loader, libraries and tools needed
by the selected runtime explicitly. Directory grants also permit execution below
those directories; aliases restrict Forge's command resolution, not everything
the payload can execute within a granted directory. There is no implicit PATH or
`/usr` grant. Do not grant credentials, the live repository or Git metadata.

Forge holds and rechecks the grant and its bound inputs, prepares an owned private
snapshot, removes its Git metadata before the payload, and runs commands under
its sandbox and owned cgroup. A recorded snapshot commit is diagnostic; the
payload does not receive Git history. Network access is denied. The actor retains
the private root when cleanup or input verification is uncertain; there is no
process-group cleanup fallback. Executable files and their ancestry must have
safe ownership and permissions; a group-writable toolchain directory is refused.
Provision a private verified runtime copy when necessary instead of weakening
that check or modifying a shared installation. For the root profile, the declared integration target
is used by both the baseline and cargo-mutants: an additional unpinned test must
not supply kills for the declared test's campaign (regression #195).
Ordinary `forge mutation inspect` reports
`NOT_READY` because it does not validate grants or exercise confinement.

Current native PASS evidence and corpus admission require a consistent owned
cleanup ledger and a removed snapshot, including the prior opt-in Cargo-value
ledger. Historical process-group-only receipts remain readable as records but
do not establish current whole-tree cleanup admission. These ledger checks are
structural consistency checks, not independent execution attestation.

Ordinary native root Cargo receipts carry a `supporting_inputs` identity with
scope `cargo-root-rust-discovery-v1`. It binds the root `Cargo.toml` and
`Cargo.lock` and the bounded inventory of Rust files in the admitted tree, excluding fixed
output roots; additions, removals, type changes and byte changes to those inventoried
inputs invalidate the identity. Selected mutation sources and tests retain their
separate campaign pins. The runner holds and compares host and snapshot inputs,
and native live corpus, committed corpus and candidate validation independently
reconstruct the discovery inventory.
If held execution inputs drift during a run, the runner refuses before further
commands and retains the snapshot for inspection even after owned process cleanup
completes. The receipt records the input-verification failure and retained path.

Older ordinary native receipts without this binding remain readable, but cannot
satisfy current native admission; regenerate them through the automatic runner.
A compatibility caller that cannot independently capture the inventory fails
closed. This identity covers bounded Rust/Cargo discovery bytes, not all compiler
inputs: non-Rust files read by macros or at runtime, toolchain and environment
closure, and producer authenticity are outside its scope. The existing opt-in
Cargo per-test value contract has its own inventory and is not replaced by this
binding.

### Ordinary workspace profile

Generate the profile through the native writer; do not edit its manifest:

```sh
forge mutation plan rust --owner my-lane --base origin/main \
  --only path/to/package/src/changed.rs --cargo-workspace --jobs 1 --run-timeout 120
forge mutation write rust --owner my-lane --base origin/main \
  --only path/to/package/src/changed.rs --cargo-workspace --jobs 1 --run-timeout 120
```

This Rust-only option cannot be combined with `--rust-value-contract`. Explicit
`--jobs 1` is required for execution; the general Rust planning default is four.
Only selected changed sources are mutated. Forge maps package-relative generator
selectors and repository-relative pins to workspace-relative engine file filters
using validated Cargo metadata. Test pins are anchors for the selected package's
whole standard test suite, not a claim of per-test attribution.

Select one standard library or integration-test target when the intended
validation scope is narrower. The native writer emits `cargo-workspace-v3`:

```sh
forge mutation write rust --owner my-lane --base origin/main \
  --only path/to/package/src/changed.rs --cargo-workspace --jobs 1 \
  --cargo-test-target lib --cargo-test-filter module::tests::
```

Use `--cargo-test-target test:contract` for a named integration target. Omit
`--cargo-test-filter` to run the whole selected target. Filters are literal
libtest name substrings, limited to 256 bytes of ASCII identifiers separated by
`::` (a trailing `::` is allowed). They are not regular expressions or exact-case
selectors. Target names and filters
cannot supply arbitrary Cargo arguments. Planning records the requested scope;
execution independently resolves the target and its source from the captured
manifest and Cargo metadata. Disabled, ambiguous, feature-requiring and custom
harness targets are refused. Required features are never enabled implicitly.

Both the direct baseline and cargo-mutants select the same build/test target.
The filter applies only to test execution. Each baseline must observe one
complete libtest frame with a passing test, and their test-name/status inventory
digests and counts must match. Candidate and corpus admission bind the recorded
selection and command arguments to the verified campaign. These remain
cooperative observations, not execution attestation or per-test attribution.
Full snapshot freshness and execution grants still apply to the narrower scope;
a selected target does not grant compiler headers, test executables or environment
variables. Existing unselected `cargo-workspace-v2` selection is preserved.
Both workspace profiles now refuse mutation subjects at the Cargo workspace
root: cargo-mutants treats slashless file patterns as recursive basename globs,
which could include unselected files. Subjects such as `src/changed.rs` remain
supported. This refusal occurs after metadata validation and before testing or
mutation; Forge does not silently broaden the requested source scope.
The [selected workspace controls](performance.md#selected-workspace-test-controls--2026-11-06)
exercise real automatic mutants with both killed and surviving outcomes,
including an unselected target that cannot compile.

Add an operator-selected repository-relative lockfile to the grant:

```json
"cargo_workspace": {"lockfile": "path/to/workspace/Cargo.lock"}
```

The discovered workspace must use that exact lockfile. Local path dependencies
may lie outside the workspace directory but must remain inside the captured
repository. External packages require a seed; Git and alternate registries are
unsupported. Prepare crates.io inputs from an explicit public cache:

```sh
forge mutation cargo-seed \
  --cache-root /absolute/public-cargo-cache \
  --lockfile /absolute/repository/path/to/workspace/Cargo.lock \
  --lockfile-sha256 SHA256_OF_EXACT_LOCK_BYTES \
  --registry-directory EXACT_OPERATOR_SELECTED_NAMESPACE \
  --cargo-sha256 SHA256_OF_GRANTED_DIRECT_CARGO \
  --output /absolute/private/new-seed
```

Use the selected cache namespace, such as `index.crates.io-1949cf8c6b5b557f`, and
independently hash the lockfile and the granted Cargo executable. The output must
be a new private directory. Copy the returned `config` object unchanged into
`cargo_workspace.seed`, then pin the final grant bytes. Run the campaign with
the normal `--allow-mutations`, exact source scope and execution-grant arguments
shown above. Never hand-author a seed manifest or change shared-cache modes.

Provisioning reads only lock-selected archives, canonical sparse-index records
and registry metadata `config.json`. It excludes Cargo-home configuration,
credentials, Git caches and host-extracted sources. Imported public files must
be regular, singly linked and world-readable. Current-user group-writable cache
inputs are treated as untrusted; private seed destinations retain strict modes.
Archive checksums are bound to held lock bytes. Index/config bytes are opaque
public-cache observations, not authenticated by `Cargo.lock`.

Snapshot identity v2 covers the captured source tree, including non-Rust data.
It uses Git HEAD/index inputs and excludes configured mutation-output namespaces
and fixed runtime outputs. Newly untracked sources must be committed or indexed
before they can be part of this snapshot. Candidate and live/committed corpus
validation independently derive the expected identity from the verified profile
and held project configuration; a receipt cannot select its own exclusions.

The baseline runs the selected package with `cargo test --locked --offline`.
The engine runs `--in-place` only inside Forge's disposable snapshot and uses the
same test package. cargo-mutants 27.1 rejects `--jobs` with `--in-place`; Forge
uses its single-worker default while requiring one worker in the campaign.
Nonzero passing libtest observations must agree between direct and engine
baselines. Full source restoration and owned cleanup are checked before disposal.

Cargo metadata paths may contain confined parent components, as in a CLI package
sharing a sibling's source. Every existing component is checked for symlinks;
paths cannot escape the admitted root and re-enter it. Missing example, test or
bench paths are allowed only for unselected packages, since published dependencies
can omit those files. Selected package targets and dependency build targets remain
required. An absent suffix cannot be followed by a parent component. Refusals
identify the package, target and path.

An exact `--only` selection ignores unrelated dirty output files when checking
ownership. Selected dirty files still require this owner's active claim, even
when their paths also appear in the committed branch diff. Unchanged selections
remain refused.

Active Cargo/toolchain configuration is refused along the command's actual
directory ancestry, including the writable scratch parent. Input checks run
before and after commands. These are point-in-time observations, not protection
against hostile same-UID transient changes. Operator read/execute roots remain
separate grants; source and seed identity do not attest all compiler/runtime
inputs. This profile adds no network, ambient Cargo home, inherited environment,
Python guest or Git payload authority.

`resources.memory_mib` controls aggregate cgroup memory, not per-process
`RLIMIT_DATA`. Explicitly positive `MUTATION_MEMORY_CAP_GIB` requests are refused
because the confined path cannot honor that legacy contract. Receipts distinguish
the aggregate enforcement from implicit or explicit legacy settings. If source
verification fails after a command, its bounded diagnostics are retained alongside
the verification failure; uncertain state retains the snapshot.

All locked registry sources are staged by Forge's bounded archive reader without
restoring timestamps or ownership. Each package's checksum, inventory and written
contents are checked before its exact Cargo completion marker is created. The
complete package set is checked around commands; missing packages also refuse.
This preserves the sandbox's metadata restrictions. Partial staging is retained
on failure. Clean-release workspace and registry-backed trials verify scoped
corpus admission, source/data drift refusal, exact restoration and owned cleanup.
A timed-out real mutant produced an incomplete ERROR receipt and retained the
unrestored snapshot after process cleanup.

The earlier package-wide Forge workload admitted all 338 registry packages but its baseline
failed while compiling `ring`: the explicit read grants excluded the required
`/usr/include/stdc-predef.h` header. A seed does not grant compiler headers.
An explicit header read grant subsequently passed an authored C compilation
control and the five-test `mutation_cargo_supporting::tests::` library baseline.
That actual campaign's engine timed out; this does not establish a completed
campaign. Other Forge tests require fixtures such as `FORGE_TEST_BIN` and Git
that this interface does not grant. The full package suite remains unproven;
see the current [engineering status](roadmap.md) and [observations](performance.md).

### Interrupted ordinary Cargo progress

After an engine timeout or abnormal stop, Forge can record
`engine_partial_progress` once owned process cleanup is confirmed. This versioned,
explicitly incomplete diagnostic binds the discovery and outcome files, reconciles
unique mutant identities against the selected source/package, and reports observed
counts and identity digests. It supports the cargo-mutants 27.1 report shape.
Identity digests use canonical JSON so nested object key order and omitted diff
bodies do not change identity. A matched observed engine timeout remains the
diagnostic reason even if subsequent source verification also fails; the original
failure and verification errors are preserved.
Each input is bounded to 16 MiB, with row and identity limits; serialized progress
is bounded to 64 KiB. Missing, malformed, inconsistent or changed reports instead
produce a bounded `engine_partial_progress_error`. Neither case replaces the
original failure or permits reading output from an unresolved process owner.

Stopped-run accounting, scored mutant rows, source coverage and survivor baselines
remain unchanged. The difference between discovered and observed mutants may
include interrupted work: `remaining_or_interrupted` stays null, not an inferred
unstarted count. Both diagnostic fields survive receipt slimming. Either field's
presence makes a PASS claim inadmissible and prevents PASS/RATCHET_HELD corpus
acceptance, including null or malformed values. These are cooperative observations,
not execution attestation; ordinary snapshot verification and retention still apply.

## Receipt format

A mutation receipt is one JSON file with two blocks (slice L):

- **Summary** — plain JSON at the top level, byte-identical to what the
  pre-slice-L receipts carried: `campaign_id`, `status`, `generated_at`,
  score and killed/survived/timed-out counts, `source_sha256`,
  `runner_components_sha256`, engine, base commit. Every reader that only
  wants to know *how a campaign went* keys off these and never decodes
  anything.
- **Detail** — the per-mutant rows (`mutants`) and the value analysis
  (`test_value`) folded under one `detail` key, in one of three shapes:

  | `detail.encoding` | When | Shape |
  |---|---|---|
  | `json` | fewer than 50 mutants and under 32 KiB serialized | the detail keys inline |
  | `zstd+base64` | everything else | one `blob`, zstd-compressed base64 of the detail object's JSON |
  | `superseded` | an older receipt of a campaign that has a newer one | `{"superseded_by": "<newer file>"}` |

One decoder owns the format: `conductor.mutation_receipt_slim.expand_receipt`
(behind it, `receipt_expand_detail_native` in conductor-native). Readers that
need detail rows call `expand_receipt_field(receipt, "test_value")` and
decompress only when the field actually lives under the block; a summary-only
reader pays nothing. `forge receipt show <path>` prints a receipt expanded for
humans. Legacy receipts (no `detail` key) pass through every seam unchanged.

The engine writes slim receipts at every disk copy (the RUNNING stub, ERROR
receipts, and the final write); the in-memory dict stays full, so attribution
and the CLI summary are unaffected. Files are written in the canonical
receipt shape (`json.dumps(..., indent=2, sort_keys=True) + "\n"`, atomic
replace) regardless of encoding.

### Compaction

`conductor mutation_receipt_compact <dir>` is one pass over a receipt
directory: every kept receipt is slimmed, and every other receipt of a
campaign has its detail replaced by a `superseded_by` pointer. Which receipt
is kept is the audit's decision, not the clock's — the CLI runs
`mutation_patch_audit`'s own acceptance predicate first (a newer receipt can
be rejected because its runner components match neither this runner nor any
lineage entry) and hands the per-campaign keep-set to the native pass, so the
receipt the audit reads afterwards is exactly the one whose detail survived.
`--no-audit-keep-set` falls back to newest-passing-status, right only when no
receipt was lineage-rejected.

### Slim receipts and the evidence gate

The native evidence gate (`verify-evidence` behind `verify_mutation_evidence_native`
and `validate_mutation_receipt_native`) validates a receipt through
`receipt_errors`, which expands a slim receipt before any rule reads it — so the
`mutants` and `test_value` lists folded under `detail` are restored for the
ratchet cross-checks, a plain pre-slim receipt (no `detail` key) validates
unchanged, a `superseded` pointer is rejected with `receipt superseded by
<newer file>` and can never rank as evidence no matter what its summary says,
and a blob that does not decode is rejected with its own decode error rather
than judged on the summary alone. Between slice L landing the format and this
seam being added, every tracked receipt was slim and the gate rejected them all
with `mutants must be a non-empty list` — a real engine PASS unreadable as
evidence; no CI job ran `verify-evidence` at the time, which is why the gap
went unnoticed for a week (the job below exists because of it).

Every rejection is classified where it is produced, never parsed back out of
its message: each `receipt_rejections` entry is `{receipt, kind, detail}` with
`kind` one of `no_campaign`, `scope_error`, `not_pass` (any status other than
PASS, `RATCHET_HELD` included — one verdict, not a wall of follow-on schema
noise — plus the two stale-PASS bindings, source drift and a manifest revision
the campaign has moved past: the run was real, it is just not current, and the
campaign re-runs), `superseded`, `runner_map_mismatch` (the component map and the
core/adapter/scope-guard era bindings: same debt, the campaign re-runs),
`decode_error`, `schema_error` (the validator refuses the receipt's claim —
including survivors outside the baseline, which contradict the PASS it
asserts), `manifest_load_error`. The old free text survives verbatim as
`detail`, and the result carries a top-level `rejection_counts` aggregate
(schema `llm.mutation-testing.evidence-check.v2`; a receipt the loader cannot
parse at all counts as `decode_error`).

### CI

Forge CI checks that the shipped receipt tools work. It does not require changed
tests to have mutation evidence or require PRs to record missing-evidence debt.

The repo-wide canary, `uv run python -m conductor.mutation_coverage
canary` (locally `make mutation-canary`) re-checks every receipt in the tree
and exits 5 when any of them is unreadable, whoever's tests it covers, and 0
otherwise — however much evidence is missing. That is the check that would
have caught the #41–#46 gap on day one.

The retention report, `make mutation-retention`, runs in dry-run mode in CI. It
prints what a sweep would keep, delete, and leave to protection, and exits:

- **0** — the sweep ran; the plan on stdout *is* the report, a non-empty
  `deleted` count included. Receipts are supposed to accumulate between
  sweeps, so an uncitable list is a plan, not a failure;
- **2** — the sweep could not decide (an unreadable or unloadable manifest,
  a gate that would not run) and touched nothing; the reason goes to stderr
  and the job fails;
- anything else is an unhandled defect — a crash like the TypeError that
  kept `make mutation-retention` dead for a week fails the step it always
  should have.

`MUTATION_RETENTION_APPLY=1 make mutation-retention` performs the deletion
locally; `MUTATION_RETENTION_ARGS="--protect FILENAME"` shelters a receipt a
lane has in flight.

### Optional host evidence checks

A host that adopts a changed-test evidence policy can run
`uv run python -m conductor.mutation_coverage changed --base "$merge_base" --github`.
It inventories changed tests (merge-base diff, ACMR), checks their receipts, emits
annotations and a step-summary table, and exits:

- **0** — every changed test file has evidence;
- **6** — evidence is missing (`no_campaign`, `not_pass`, `superseded`,
  `scope_error`, or `runner_map_mismatch`); this is not a PASS receipt;
- **5** — a validator-side rejection (`decode_error`, `schema_error`, or
  `manifest_load_error`);
- **4** — REFUSED because of registry or campaign errors.

The host decides how these results affect its workflow. Locally,
`make mutation-evidence MUTATION_BASE=<ref>` runs this optional check against any
ref (`origin/main` by default). It is not a Forge landing requirement.

### The native runner

`forge mutation run <manifest> --allow-mutations` runs a generated campaign in a
disposable snapshot, then proves the pinned sources and tests held in both the
snapshot and the live repository before it scores anything.

Snapshot preparation uses native Git object APIs and raw file capture. It does not
launch Git, run checkout hooks, invoke clean/smudge/process filters, or install
templates. It overlays current bytes of HEAD-tracked files and the eligible
untracked inventory into a private object database and synthetic commit; the host
index and objects are not written. A staged addition is already indexed and is
therefore not an untracked overlay. Executable modes and supported relative symlinks
are preserved. The private repository may read parent objects through an alternate.

Default Git ignore files, `info/exclude` and per-directory `.gitignore` files are
parsed as bounded held data with normal rule precedence. HEAD, index, named Git
routing, directory identities and rule inputs are rechecked. Unsupported conversions
and configurations refuse explicitly: attributes files, config includes, custom
attributes/excludes paths, sparse or conflicted indexes, intent-to-add entries,
gitlinks and unsafe routing overrides. Missing optional rules must remain missing.
This avoids silently substituting raw bytes for a configured conversion.

Capture is bounded to 100,000 scanned entries, 32 MiB per source blob and 2 GiB of
source data. It is a serialized, point-in-time capture, not an atomic filesystem
snapshot. Libgit2 repository discovery can read metadata before the bounded
configuration checks; no universal preemptive filesystem-I/O deadline is claimed.
Engine process ownership is separate: the opt-in Cargo actor uses owned cgroups;
legacy engine descendant cleanup remains tracked in
[#192](https://github.com/mcpirate17/llm-forge/issues/192).

| exit | meaning |
|---|---|
| 0 | `PASS` or `RATCHET_HELD` |
| 1 | a verdict that is not green: `FAIL`, `ERROR`, `INCOMPLETE` or `CANCELLED` |
| 4 | refused, with `{"status": "REFUSED", "error": ...}` on stdout |

`forge mutation inspect` exits 0 for `READY`, 3 for `NOT_READY` and 4 when it cannot
read the manifest.

Every receipt carries `accounting`, with `planned`, `completed`, `failed`, `censored`
and `unstarted` counts, and `complete_campaign`. `planned` comes from the engine's own
total, so a run that stopped early shows the gap. An unfinished engine, any
`TIMED_OUT`, `NO_COVERAGE` or `UNVIABLE` row, or an unstarted mutant prevents a
complete campaign and yields `INCOMPLETE` unless a new survivor or row error
already requires `FAIL` or `ERROR`. Known survivors cannot hide these gaps as
`RATCHET_HELD`. Cargo-mutants exit 2 is a completed survivor verdict only with a
reported survivor; other engine failures remain incomplete. `CANCELLED` means a
signal stopped it; `unstarted` stays null if the engine
had not yet said how many it planned. Neither is PASS evidence. A cancelled run never
records a survivor baseline, and neither does a run with unstarted mutants. An
`INCOMPLETE` verdict that reaches scoring with every planned mutant reported can
record one. That cannot weaken the ratchet: a censored mutant is not in the
baseline, so if it survives later it counts as a new survivor and the run is `FAIL`.

All three native engines require at least one scored (`KILLED` or `SURVIVED`)
mutant for **each selected source**. A versioned `source_coverage` report records
per-source counts and missing files. One measured source cannot conceal another
source with zero mutants: the run is refused (exit 4), its receipt is `INCOMPLETE`
with `complete_campaign: false`, and no survivor baseline is recorded. An engine
exclusion cannot waive a selected source. Supporting hashes outside the selected
source set are refused as ambiguous scope; the runner never expands mutation
authority to cover those files. Distinct symlink aliases are unsupported; redundant
dot segments are normalized.

Native validators independently recompute this report from source pins and mutant
rows. The scorer also checks the campaign's exact source pins. Missing or forged
counts cannot establish completion. These checks prove a nonempty measured mutant
set per selected file, not per-line coverage or exhaustive mutation operators.

The Cargo adapter spells a root-package manifest as `./Cargo.toml` when invoking
cargo-mutants, preserving a nonempty parent directory without modifying the
generated manifest. Nested and absolute manifest argument spelling is preserved.

Python remains necessary for fest: its interpreter must import `conductor` for
the `mutation_pycache_evict` pytest plugin and have `pytest-cov` (the `mutation`
extra), or the baseline fails on `--cov`. `[tool.conductor].snapshot_python`
selects that interpreter; without it, fest retains host-venv/PATH discovery.

Rust and Mull campaigns do not discover or require Python by default. An explicit
`snapshot_python` selects a guest interpreter for tests that need one. Without
that setting, the receipt records `interpreter: null`, and supervised engine,
test and attribution children cannot inherit a stale `CONDUCTOR_SNAPSHOT_PYTHON`
from either the caller or campaign.
An explicit selection owns that environment key. Native bytecode-cache isolation
remains active for guest tests; it creates directories and environment settings
without invoking Python. Invalid configuration types and blank selections fail
explicitly. The configured path or command is recorded, not its executable bytes;
this selection is not interpreter binary attestation. External tools and project
tests still require their own declared toolchains.

A receipt names its runner with `runner_kind: forge-native` and a `native_runner`
block in place of the Python runner's hashes. A build whose native fingerprint
inputs differ from HEAD records `source_matches_git: false`, and validators reject
its receipts. Unrelated dirty documentation does not change that stamp. Evidence
requires committed native inputs.

Native-runner schema v2 compares the compiled source fingerprint with the audited
native source inventory: source trees, Cargo manifests/lockfiles, build scripts
and declared embedded assets. The former v1 partial component list is rejected;
lineage cannot waive a stale native fingerprint. A source-less host needs the
explicit [installed native pin](#installed-native-runner-identity) for native
candidate/corpus validation; missing sources alone never establish identity.
This is cooperative source/build/binary consistency, not authenticated producer
identity, compiler equivalence or binary attestation.
Captured candidate/corpus inputs retain their own freshness checks; ambient
compatibility reads remain filesystem observations.

Native process capture retains at most 16 MiB of stdout and 1 MiB of stderr.
It drains excess bytes so children can exit, then refuses completion; overflow,
read errors and reader failures cannot turn truncated output into a successful
run. Existing engine deadlines still bound execution.

### Ratchet iterations

`make mutation-engine-run` resolves the receipt root through
`forge project --host ROOT path mutation-receipt-root` and writes beneath its
`.iterations/` directory (`campaigns/receipts/.iterations/` in this repository).
`MUTATION_RECEIPT` and `MUTATION_RECEIPT_ITERATIONS` remain explicit overrides;
configuration failures stop the target before execution. The Make runner leaves
directory creation to the native runner after path confinement. A ratchet loop that commits
every iteration was growing the tracked tree by ~350 KB per run, and every
clone and CI checkout paid for it. When a loop settles,
`make mutation-receipt-promote` copies the newest iteration receipt into the
tracked directory; only that copy is committed. Promotion remains an
operator-controlled shell maintenance adapter using `mkdir`/`cp`, without the
native publication confinement or no-overwrite guarantee; its migration is pending.

Campaign identifiers are single filename components, at most 200 UTF-8 bytes,
starting with a letter or digit and containing letters, digits, dots, underscores
or hyphens. Native campaign and receipt publication reject parent traversal and
resolved paths outside the repository before creating output directories. Temporary files use exclusive
creation; unforced campaign publication cannot overwrite a concurrent winner.
Configured ancestors remain trusted against concurrent replacement.

### Bytecode isolation

Every child command an engine spawns — the baseline suite, the engine binary
(and the per-mutant test commands it spawns in turn), attribution's re-applied
mutants — and the gate's two pytest collect probes run against a
`PYTHONPYCACHEPREFIX` scratch that is private to the run, wired once in the
runner (the choke point all engine children flow through) and
`gate.preflight_pytest_config`. CPython validates a
`__pycache__` entry by source mtime and size, so a same-size rewrite within
one mtime second leaves a stale `.pyc` that later interpreters keep
executing — and applying a mutant *is* a same-size same-second rewrite, which
let a campaign grade the unmutated bytecode and report a survivor no source
diff explained (PR #49's spurious `arithmetic_op`). With a prefix set,
CPython stops reading beside-source caches entirely, so a stale one cannot be
read even where it already exists.

Only the mutated file can be stale — every other module is byte-identical
across the mutants of one run — so PR #53 caches instead of refusing to write:
`PYTHONDONTWRITEBYTECODE` is gone (PR #51's version set it, making every
child recompile every module and costing the retention campaign 2.6x wall
time), unmutated modules compile once into the run's prefix and are served
from it after, and each mutated source's cached `.pyc` — plain, `.opt-1` and
`.opt-2`, the paths `importlib.util.cache_from_source` maps — is evicted
before every child launch that must not trust it (`isolated_python_env`).
fest's subprocess backend builds each mutant's pytest command itself, so no
launcher sits between the rewrite and the import; the adapter loads a pytest
plugin (`conductor.mutation_pycache_evict`, via `PYTEST_ADDOPTS`) that repeats
the eviction inside every child before collection imports anything, and
attribution evicts around every re-applied mutant. Measured on
`conductor-67_mutation_retention_fest_20260913` (`MUTATION_BASE=ff91c95`,
outcomes and survivor set identical in every run):

| | wall clock (median of 3) | KILLED/SURVIVED/NO_COVERAGE |
|---|---|---|
| pre-#51, no isolation (single run) | 51.1 s | 90/8/19 |
| #51, refuse to write (single run) | 131.8 s | 90/8/19 |
| #51 as merged (3 runs) | 127.67 s | 90/8/19 |
| #53, cache + evict (3 runs) | **51.12 s** | 90/8/19 |

### Orphaned runs

The runner starts every engine in its own session and binds it to the
engine process's lifetime with `PR_SET_PDEATHSIG`, so the kernel kills the
engine binary the moment the process that spawned it dies (Ctrl-C, session
end, OOM, sandbox teardown) — but the mutants that engine already spawned
inherit nothing and may keep running. The native runner serializes registration
and removal under a persistent interprocess lock. Its separate
`native_pgids.json` uses a `native` array and preserves diagnostics when process,
pipe or capture cleanup fails. A recorded numeric PID/PGID is not authority to
signal a later process: manual recovery must establish current process identity.

Legacy Python runners use `live_pgids.json` with a `live` array. The legacy
`make mutation-reap` command does not consume native records, including with an
older installed Python adapter. Legacy native supervision handles its original
process group on ordinary completion, timeouts and cancellation. A detached
descendant can escape that group even on normal completion; this is not proof of
whole-tree cleanup (see #192 above). Verified automatic recovery after a hard-killed
native supervisor remains a separate capability to implement.


## Contextual new-test admission

Ordinary `verify-evidence` keeps its canonical receipt projection. A host may
add exact node/source requirements under `[tool.conductor.mutation_value_contexts]`:

```toml
[tool.conductor.mutation_value_contexts]
"tests/test_flow.py::test_flow" = ["pkg/training.py", "pkg/selection.py"]
```

These requirements are additional obligations, not waivers. Each required source
needs a current registered automatic PASS original, independently checked by the
unchanged native scope, manifest, source/test hash, runner and provenance rules.
The test must have CORE or INTENTIONAL_REDUNDANCY value and actually kill a
reported mutant in that source. Every reported kill must be attributed; baseline
failures, survivors, errors, timeouts and missing value evidence block admission.
Unrelated positive source contexts cannot satisfy the declaration.

The latest attempt is selected separately within each exact final context. A
failed or partial latest attempt prevents fallback to an older PASS in the same
context, and contradictory classifications fail closed. A newer MERGE receipt
in another source context cannot erase a valid required witness. MERGE rows
remain in the original evidence. Candidate review retains the original receipt
paths, byte hashes, bindings and value rows in
`contextual_mutation_witnesses.json` in its runtime artifacts. It never writes a
combined receipt or changes a classification. Hosts without declarations retain
canonical value admission and now also reject incomplete nominal PASS receipts.
