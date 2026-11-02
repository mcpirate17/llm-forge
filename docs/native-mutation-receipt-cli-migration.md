# Native mutation receipt target-tree verification

Use `forge receipt verify --receipt FILE --tree COMMIT_OR_TREE [--repo DIR] [--json]`.
This replaces `python -m conductor.receipt_verify`; it is separate from
`forge candidate verify-receipt`, which checks candidate-review receipt digests.

The four checks reuse `conductor_native::receipt_auth` unchanged in meaning:

1. The receipt's normalized manifest path names a blob in the resolved target tree.
2. The blob's actual SHA-256 equals the receipt's manifest SHA-256.
3. Every source pin declared by that manifest matches actual target-tree blob bytes.
4. The digest of the receipt's recorded source-pin inventory matches the inventory
   reproduced from actual target-tree bytes over the manifest's pin paths. Lines
   retain `<sha256>  <path>\n` formatting and hash-first/path-tiebreak ordering.

A target-tree binding PASS exits0. Measured missing blobs, hash differences and
inventory divergence exit1. Malformed receipt/manifest, unusable repository/ref,
or an input limit returns REFUSED/exit4. Ordinary command-line usage errors retain
Clap's exit2. Resolved repository/tree lines precede verification; JSON mode puts
them on stderr, leaving stdout one verdict object. Repository discovery defaults
to cwd and accepts a subdirectory. Commit, tag and raw tree references peel to the
actual measured tree. Pasted smart dashes retain the old leaf's repair behavior.

All manifest and source bytes come from Git's object database, never the host
checkout/index or receipt-relative filesystem paths. A symlink-mode tree entry
is still a blob whose stored bytes are hashed; its contents are not followed as
a host filesystem link. The explicitly selected local receipt is a bounded
regular file; final symlinks and special files now refuse through Forge's shared
regular-file reader. Duplicate JSON keys also refuse rather than select a value.

Limits: receipt32MiB; manifest2MiB; each source blob32MiB and aggregate blob reads
64MiB; source pins4096, normalized path length4096/depth256, aggregate pin path
bytes1MiB. Returned/post-resolution commit and tag metadata admit1MiB each,
with at most32 native post-resolution annotated-tag hops; tree metadata admits8MiB per tree,4096 relevant
trees,100000 entries and64MiB aggregate. Source-observation reasons/path bytes4MiB;
verdict JSON, including pretty output,16MiB. These are semantic object/result
limits, not promises about libgit2's intermediate allocation. Limits refuse
explicitly and never omit pins to produce a pass.

All repository opening, revision evaluation and Git object reading occur in a
same-native-binary worker. Normal packed objects/packed refs and existing libgit2
revision expressions are retained; there is no loose-only fallback or new Git
pack/revision parser. A supervisor owns its process group, cancellation, timeout
and reaping. Hard kernel ceilings are2GiB RLIMIT_AS and30 CPU seconds, clamped to
lower inherited hard limits; the supervisor and a default/unblocked worker alarm
apply60-second wall deadlines. RLIMIT_AS limits virtual address space, not measured
RSS. Uninterruptible kernel I/O can delay teardown and still requires an outer
watchdog, as with existing native process supervision.

A private anonymous result memfd is capped at16MiB+4096 framing bytes and sealed
before acceptance. Only that descriptor plus stdio survives native re-execution;
normal worker diagnostics have a strict combined64KiB cap. Direct hidden worker
entry applies and kernel-verifies limits too; it requires an empty owned anonymous
sealable read/write descriptor, never an ordinary inherited file/stdout channel.
No environment token bypasses supervision or establishes authority. Result status,
complete sealed frame and observed kernel ceilings must agree before0/1/4 verdict
handling. Malformed/partial/unsealed output, resource death, cancellation, timeout
or supervisor failure is REFUSED/4, never campaign success. No disk scratch file
or interpreted helper is created.

libgit2 revparse can load and parse objects before returning; packed deltas can
materialize a larger base than the final object's header size. Consequently the
8MiB tree check is semantic admission, while whole-worker kernel limits contain
such allocation. Revision expressions can themselves peel/walk objects before
returning (for example `^{tree}`); the post-resolution hop/commit/tag limits do
not claim to constrain those libgit2-internal steps. Returned commit/tag peeling
and relevant path reads also check
headers, but header APIs are not advertised as a universal preallocation fence.

This command does not run a mutation engine, require all mutants KILLED, authenticate
historical execution, inspect approval, or certify campaign PASS. The original
automatic fixture has NO_COVERAGE outcomes and is still refused by the strict
campaign verifier. Its unchanged bytes can nevertheless satisfy these four
independent target-tree bindings. No new campaign or PASS receipt artifact is
constructed by the replacement tests.

## Coverage and retirement

Default native integration target: `native_mutation_receipt_verify_cli` (20 cases).
The seven interpreter-driven `python_contracts_receipt_verify` cases are replaced:

| Retired observation | Native replacement |
| --- | --- |
| Human four-part pass, resolved repo/tree before verdict | `original_automatic_bytes_bind_all_four_parts_without_certifying_campaign_pass` |
| Raw tree OID and all four named PASS checks | Same real automatic corpus case |
| JSON resolution diagnostics on stderr | `json_resolution_uses_stderr_and_default_repo_discovers_from_subdirectory` |
| Empty receipt refuses/exit4 | `malformed_receipt_manifest_and_unusable_context_are_refused_with_exit_four` |
| Unresolvable tree refuses/exit4 | Same malformed/context case |
| Executable module entrypoint | All replacement cases launch the actual installed Forge binary with PATH empty |
| Default cwd subdirectory discovery | JSON/default-repository case |

The historical handmade two-pin fixture is not copied. The unchanged automatic
corpus has one manifest source pin; the existing pure `receipt_auth` predicates
retain all per-part failure cases, and a typed observation-only count regression
checks two observed blobs. This is a pure count/digest test, not a campaign receipt.

Additional real CLI cases distinguish manifest hash drift from receipt-map lies,
missing sources/manifests, dirty host/index bytes, duplicate JSON and read bounds,
symlink-mode blobs, annotated tags, smart dashes, and ordinary candidate receipt
format refusal. No test invokes an interpreter or child Git command.

The old CLI module, exact PyO CLI test target, Cargo declaration and seven
source-to-target registry rows are retired. Optional PyO functions in
`receipt_auth` remain available and share the same native predicates. No other
lineage, claims-view, mutation engine, or candidate-review caller is removed.

The follow-up cases repeat full OIDs, named refs, nested tags and revision
expressions through actual native-generated packfiles and packed refs, check
large initial tree/commit/tag refusal and32/33-tag boundaries, verify an actual
sealed direct-worker kernel-limit frame, and force a private result-channel
failure through an inherited32-byte file-size ceiling. They reuse original
automatic receipt/manifest/source bytes. Oversized Git objects are negative
metadata inputs, not fabricated mutation campaign PASS evidence.

Integration validation exercised all 20 installed CLI cases, the eight native
receipt predicates, and the same eight predicates with the optional Python
feature compiled and linked against CPython 3.12. The native executable's shared
library inventory contains no libpython. The optional-feature check establishes
build/link compatibility; it is not a claim that every remaining Python caller
has been migrated or executed.
