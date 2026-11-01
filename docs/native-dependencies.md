# Native dependency audit — 2026-10-31

`cargo-audit-audit 0.22.2` checked the three complete lockfiles against RustSec
commit `2295466c4e9df26647fd67d8fa1c705ac2fc7f81` (1,395 loaded advisories).
The first Forge audit fetched the database; subsequent checks used `--no-fetch`.
No advisory was ignored or suppressed.

| Lockfile | SHA-256 | Result |
| --- | --- | --- |
| `native/forge/Cargo.lock` | `0b3eb3262e6545f10ed4f793e4c2190772a506714f8fd0955350e2a088149043` | Exit 0; one unmaintained dependency warning |
| `native/conductor-native/Cargo.lock` | `72a2dd465e111903e89af5247d7132a6b0c549d4771d34d725b9f9770c2f99f3` | Exit 0; no warnings |
| `native/slop-core/Cargo.lock` | `27c7fbbb38b53231ac020a4651d53364aa0cd97be0cda0754c109d6a0daf9c6c` | Exit 0; no warnings |

The initial Forge/conductor locks failed on four Zstandard advisories. Upgrading
`zstd` to 0.14.3 selected `zstd-safe` 8.2.0 and `zstd-sys` 2.1.2. The advisory
records cover [context reuse](https://rustsec.org/advisories/RUSTSEC-2026-0391),
[legacy streaming errors](https://rustsec.org/advisories/RUSTSEC-2026-0403),
[seek offsets](https://rustsec.org/advisories/RUSTSEC-2026-0398) and the
[Wasm allocator](https://rustsec.org/advisories/RUSTSEC-2026-0410). An affected
lockfile does not prove those vulnerable functions are reached by Forge.

Tokio moved from 1.53.1 to 1.53.3 to address its
[AsyncFd I/O-safety advisory](https://rustsec.org/advisories/RUSTSEC-2026-0386).
Forge now uses explicit unsafe registration at each reactor boundary and records
why the owned descriptor remains open and unchanged throughout registration.

`paste` 1.0.15 remains a tokenizers 0.23.2 build dependency with an
[unmaintained warning](https://rustsec.org/advisories/RUSTSEC-2024-0436).
It is not suppressed and remains visible in subsequent sweeps. These results
cover the recorded locks and database, including optional locked dependencies;
they are not a clean-host build receipt or a general security certification.
