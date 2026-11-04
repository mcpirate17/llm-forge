# Migration and evidence status snapshot

Historical material moved from the pre-rewrite architecture document on 2026-11-02.
This preserves earlier working-tree claims and remaining-work notes; it is not a
current release receipt, active inventory or deployment claim. Use the named
committed verification artifacts and current caller closure when assessing a release.
Do not add migration counts or benchmark diaries back to the component architecture.

Original architecture SHA-256: `f8d760371409fc7327251017e5973fc4b58b22d61f996e100ba81a77bf8c5052`.

## Evidence and what it establishes

Evidence has three distinct levels:

1. **Focused tests and fixtures** exercise specific semantics and failure
   boundaries. They do not establish full integration or clean-host install.
2. **Workspace suites and real owned Stage tests** exercise current native
   composition at the reported working-tree revision. They do not establish a
   committed source identity or full local-check receipt.
3. **Committed full-gate receipts and clean-host sweeps** bind evidence to an
   exact tree and declared environment. Only these support claims about the
   corresponding committed release gate; they still do not imply unmeasured
   model quality, external service behavior or production deployment.

Before committing the native review batch, `cargo test --release` passed
2,283 Forge tests with 104 environment-dependent cases ignored. Separate
explicit runs passed 13 workspace/sandbox cases, six review CLI cases, and two
Torch Stage cases. These exercise captured imports, missing `/proc`, Torch
shadowing, resource cleanup, and fresh receipt/report publication. The conductor
library passed 515 default and 555 optional-binding tests; Slop passed 187
default and 201 optional-binding tests. Another 21 mutation-value compatibility
cases passed using the freshly built external conductor extension. These
overlapping selections are not additive totals. Full-gate receipts must still
be bound to the committed tree by `forge check --all` and `forge verify --require-all`.

Focused validation of the native gate, branch/exposure and explicit Ruff fix
batch passed 610 Forge library tests, 552 conductor source-analysis library
tests, 11 branch-policy CLI cases, seven fix admission cases, and ten explicitly
provisioned fix execution cases including native Ruff 0.16.5. Two actual Cargo
fixtures passed source/build-input and Git-ref freshness checks. These are
overlapping working-tree selections, not full-gate or clean-host evidence. The
eight opt-in gate execution cases still require separate validation with a clean,
source-matching executable. Committed full-gate evidence remains in the generated
check receipts, not in these test counts.

Mutation review defaults to strict behavior. Portable grandfathering requires an
external operator grant pinning the repository commit, tree, inventory path and
inventory SHA-256. Candidate policy cannot authorize its own exemptions.
A positive external-invariant waiver still requires a genuine automatic receipt
bound to a manifest declaring that invariant. That receipt evidence remains
outstanding.

Performance data must identify binary/source hashes, runtime, hardware,
workload, warmup, repetitions and included operations. Values in this document
are bounded examples from linked contracts, not universal guarantees. Every
microsecond or nanosecond value excludes process/model costs unless the
measurement explicitly includes them. No unconditional microsecond whole-task
latency claim is supported.

## Remaining migration

The native migration remains open. The live source inventory is 83 Python
files under `src/`, including testdata fixtures; that count says nothing by
itself about reachability,
behavioral coverage or runtime usage. Remaining work is tracked by live
callers and installation boundaries, not a stale count of old milestones.
Requirements A–E remain system-level obligations: native components and focused
suites cover portions of them, while repository-wide callers, install paths and
clean-host operation still require migration and evidence. In particular, the
hardware-acceleration requirement is conditional on measured embedded workloads;
no unconditional GPU implementation is a completion criterion.

The remaining program is:

1. **Retire interpreted callers.** Find each reachable Python implementation,
   command wrapper, hook fallback, provider/bootstrap adapter and package
   surface. Move its contract to the native library/CLI and migrate callers
   before deleting the implementation. Compatibility shims are transitional
   migration aids and must also be retired; the final shipped core has zero
   interpreted dependencies.
2. **Close governance and mutation surfaces.** Inventory remaining mutation,
   waiver, audit, policy, package and repository operations. Reuse native
   algorithms; expose explicit bounds, provenance, ownership and failure
   results. Preserve mutation-engine rules and never treat a receipt hash as
   approval or execution proof.
3. **Finish packaging and clean-host paths.** Remove interpreter discovery
   from default native builds; declare shipped dependencies in package
   metadata; replace Python bootstrap/install/check dependencies only after
   native replacements are exercised from a clean install. Keep test-only
   dependencies in test configuration.
4. **Prove integration on an exact tree.** Run the required full local check
   and verification after the final source changes, bind receipts to the
   committed tree and remote main, resolve required review, and complete the
   pull-request delivery required by the repository contract. Current working-tree counts do not
   satisfy this step.
5. **Reconcile documentation.** Keep this file architectural and current.
   Preserve detailed limits, commands, measurements and historical evidence in
   focused contract documents and the migration-history archive. Remove stale
   remaining-count claims when their inventory is no longer current.

No chronological implementation diary is normative. Historical commit-by-
commit results remain available in [native migration history](native-migration-history.md)
and [native migration history archive](native-migration-history-archive.md).


Further chronology remains in [migration history](native-migration-history.md)
and its [archive](native-migration-history-archive.md). Current product evidence
belongs in [the value audit](benefits.md) and surface contracts.
