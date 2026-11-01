# Native offline Cargo audit

`cargo-audit` is a native builtin in `forge candidate execute-batch`. It finds
the nearest `Cargo.lock` for every selected candidate file, deduplicates and
sorts the locks, then evaluates them with RustSec 0.33.0 compiled without its
network features. It runs no external analyzer or interpreter and reads no
candidate `.cargo/audit.toml`, Cargo home, passwd entry or ambient toolchain.

The check covers known RustSec advisories and yanked versions for crates.io
packages. Workspace, Git and alternative-registry packages are listed explicitly
as outside this scope. Missing registry versions or checksums are errors, even
when another package or lockfile can be audited. Vulnerabilities become critical
findings; informational advisories and yanked versions become medium findings.
The policy assessor determines which severity blocks a review. Batch success is
still not a complete review or approval receipt.

Builtin `cargo-audit` policies must explicitly set `cache=false`. The native
policy parser rejects both `cache=true` and omission, so the remaining legacy
dispatcher cannot reuse a result without rechecking the evidence pin and age.

## Operator evidence

The operator grants file supplies this additional field:

```json
{
  "cargo_audit_evidence": {
    "path": "/absolute/operator/audit-bundle.json",
    "sha256": "<lowercase SHA-256 of the exact bundle bytes>",
    "max_age_seconds": 86400
  }
}
```

The bundle is a versioned JSON document:

```json
{
  "schema_version": 1,
  "captured_at": 1793550000,
  "advisory_commit": "<40 or 64 lowercase hexadecimal digits>",
  "advisories": [
    {
      "path": "crates/example/RUSTSEC-2021-0001.md",
      "text": "<complete RustSec advisory Markdown beginning with ```toml>"
    }
  ],
  "registry": [
    {
      "name": "example",
      "version": "1.0.0",
      "checksum": "<64 lowercase hexadecimal digits>",
      "yanked": false
    }
  ]
}
```

The producer supplies the complete intended RustSec advisory snapshot and exact
crates.io records needed by the candidate locks. Its commit, capture time and
completeness are operator assertions. The SHA-256 pin binds the supplied bytes;
it does not authenticate upstream provenance or prove a producer omitted no
advisories. This command does not fetch, refresh, or manufacture evidence. An
empty advisory set is refused. Each crates.io lock entry needs an exact name,
version and checksum match in the bundle before its yanked state is accepted.

The bundle must be outside both source and candidate snapshot. It must be an
absolute path without symlink components, a regular single-link file owned by
the effective UID, and not writable by group or others. The operator must keep
its ancestors immutable while loading. The loader checks descriptor/path
identity and size before and after reading, verifies the digest before parsing,
and enforces age against the real UTC clock. Future dates are refused; the
operator age limit is between one second and 31 days. `--today` affects candidate
policy planning, not evidence age.

Dry runs validate the bundle and lock ownership without evaluating findings.
No-file policy skips need no audit evidence. Execution uses private candidate
snapshots and checks cancellation around each bounded lock evaluation. Native
evaluation uses the existing owned builtin worker; it does not spawn a child
process or create a cgroup. A cancellation can wait for the current bounded
native evaluation to finish. Snapshot cleanup finishes before a result returns.

## Bounds and recorded observations

Admission caps selection at 100,000 paths, 8 MiB of input paths, 4,096 bytes per
path and depth 128. Ownership discovery caps work at one million probes and
256 distinct locks. Paths are canonical relative paths; a Unix backslash stays
literal. Missing, nonregular and symlink inputs fail. Every selected file needs
an owner; partial ownership is no longer silently discarded.

The evidence file is at most 32 MiB with at most 10,000 advisories of 128 KiB each
and 100,000 registry versions. Locks are at most 8 MiB each with a shared 32 MiB
read allowance. Admission permits 64 patched/unaffected requirements per advisory,
256 UTF-8 bytes per requirement, and a shared one-million budget for the sum of
squared requirement counts. Front matter must be accepted by the bundled TOML
0.8 parser. Every ASCII digit run in a requirement, including prerelease/build
identifiers, must fit below `u64::MAX - 1024`; larger valid SemVer identifiers
are explicitly unsupported. Rejected advisories fail the entire bundle.

RustSec validates version ranges during bundle admission and its computed ranges
are retained.
Across all locks, matching charges one unit per package, one per candidate
advisory and 34 per cached range against a one-million-unit budget. This is a
conservative work proxy, not an instruction count or wall-clock guarantee.
Advisory names and source strings are capped at 128 and 4,096 bytes respectively.
Findings, observations and final JSON have explicit limits;
escaping is included when measuring serialized output. Oversized data fails.

Results record the owner mapping, each lock observation, out-of-scope sources,
RustSec version, evidence digest, advisory commit, capture and check times, and
work accounting. Withdrawn advisories are excluded by RustSec's query semantics;
there are no implicit advisory ignores or target architecture/OS filters.

## Remaining legacy bridge

The Python audit wrapper is deleted. The remaining legacy review dispatcher
forwards selection metadata to this same Rust evaluator. It requires
`FORGE_CARGO_AUDIT_EVIDENCE` to name a separate operator-owned specification JSON
containing `path`, `sha256`, and `max_age_seconds`; that spec has the same path and
ownership restrictions and a 64 KiB limit. A missing specification fails.

Native Cargo lint uses the same explicit Cargo grant and private runtime model
documented in [the Cargo lint contract](native-cargo-lint.md). The Python lint
wrapper and ambient toolchain-recovery shim have been retired; candidate policy
routes fmt/clippy command and version calls through the uncached native Forge
stage. This lint path is separate from the legacy audit dispatcher described
above.

## Measured API latency

The release benchmark on 2026-11-01 used an AMD Ryzen 9 9950X3D, Linux
7.0.0-38-generic and Rust 1.98.0. Five warmups preceded 31 samples of 20 calls
each. Semantic checks independently verified 16 vulnerable versions and one
yanked version in the positive fixture, and no findings in the negative fixture.

| Operation | Median sample mean | p95 sample mean |
| --- | ---: | ---: |
| Parse one advisory and 32 registry records | 11.223 µs | 11.722 µs |
| Audit 32 versions with the prepared bundle | 60.647 µs | 61.202 µs |
| Audit 16 patched versions with the prepared bundle | 28.679 µs | 29.949 µs |

These are current-only synthetic API measurements on an unisolated warm host,
including result destruction. They exclude evidence admission, filesystem,
network, sandbox and CLI costs; they establish neither an end-to-end dispatch
latency nor an improvement over a baseline. No timed sample failed or exceeded
the cooperative deadline. The [raw measurements](../research/performance/native-cargo-audit-20261101.json)
include workload/output hashes. Reproduce with:

```sh
CARGO_BUILD_JOBS=2 PYO3_PYTHON=/nonexistent/forbidden-python \
  cargo +1.98.0 bench --offline --locked \
  --manifest-path native/conductor-native/Cargo.toml \
  --features bundled-sqlite --bench candidate_cargo_audit
```

Measured executable SHA-256: `b3b018f8e8c4d8e1bab4a98f1654ed40aca60ec016071f8920ed61224ff51f03`.
Benchmark source SHA-256: `412b9b4bb080495b8d8a7672e76d2f6943245767ea17f732a95ca2ef5bd6ec20`.
Engine source SHA-256: `f5a511458f96de449b8d0842522c8f6acd60f1d0457fb1d5e1c78910cea6713e`.
Dependency lock SHA-256: `5301ab10b78b9fcece7035d1f54c07d0124eb10c8f175067c7c2b682d217b14b`.
