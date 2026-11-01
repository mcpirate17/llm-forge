# Native candidate Cargo lint

Forge provides `forge candidate cargo-lint --mode fmt|clippy [--execute | --tool-version] -- FILE...`. Planning is the default: it reads the captured roster and candidate files, prints a JSON plan, and runs no compiler. `--tool-version` probes the explicitly provisioned native Cargo toolchain. `--execute` runs the planned manifests sequentially. This command can be invoked directly, but direct execution is not sandboxed; governed product execution must call it as a granted external stage through `forge candidate execute-batch`.

The roster is resolved from authoritative `forge.toml`, with `pyproject.toml` as fallback; `CONDUCTOR_CRATE_ROSTER` explicitly overrides the configured roster location. Missing, malformed, or incompletely classified inventories fail closed. The IDs remain `cargo-fmt` and `cargo-clippy` in candidate policy. Their commands and version probes route through the native Forge CLI as granted external stages, with `cache = false`.

## Selection and captured scope

The planner maps sorted selected paths to their nearest owning manifest. It inventories bounded manifest paths, checks tested/excluded classification, then conservatively expands workspace and borrowed-target scope before applying style/lint filters. The result includes original input owners, added manifests and why they were added, and visible skips. `fmt` includes every selected crate except `unstyled`; `clippy` includes only `linted` crates. A prerequisite skip marks the result incomplete (exit 2), but other selected crates still run. Intentional unstyled/not-linted skips do not make it incomplete.

Workspace selection includes inventoried packages below the workspace root and explicit members, minus excluded members. Explicit member patterns must match captured manifests. Selecting a member with `package.workspace` validates the referenced workspace without expanding its siblings. Captured `lib.path`, bin/test/example/bench paths and `package.build` add borrowing packages when an input lies under a borrowed target's parent, or a selected directory contains that parent. Known local dependencies must be captured, confined, and identify packages; they do not themselves schedule additional lint owners. Arbitrary package metadata is inert. This is bounded static scope discovery, not Cargo validation or compiler/build-script attestation; implicit ancestor workspace search, Rust `#[path]`, and arbitrary build-script behavior are not inferred.

Scope reads use the stable manifest reader: 1 MiB per `Cargo.toml`, 8 MiB aggregate. Component-aware glob traversal and manifest probes share finite work budgets; scope expansion is capped at 10,000 records, retained paths at 24 MiB, and exact serialized plan output at 24 MiB. Existing path, inventory, node/probe, and work bounds still apply. Exceeding an admission bound refuses planning instead of silently narrowing scope.

## Executed mode

`FORGE_CARGO_BIN` must name the canonical absolute toolchain **bin directory**, containing native ELF `cargo`, `rustc`, `rustdoc` and either `cargo-fmt`/`rustfmt` or `cargo-clippy`/`clippy-driver`. These executables must be owned by the effective UID or root and have no group/other write bits. Rustup shims and candidate-provided tools are refused. The operator must also supply canonical `TMPDIR`; Forge creates private mode-0700 child working and Cargo-home directories and per-manifest target directories. Clippy uses `--offline --locked`; no ambient toolchain recovery or Cargo search occurs.

Optional `FORGE_CARGO_EXTRA_PATH` contains up to 16 canonical helper directories, for example `/usr/bin` for `cc`, `ar`, or `git`. It is a search path, not a per-executable allowlist. Clippy alone optionally copies `FORGE_CARGO_HOME_SEED` sections `registry/cache`, `registry/index`, `git/db` and `git/checkouts`; fmt and version probes do not copy caches. Cargo credentials, configuration, extracted registry sources and installed binaries are excluded. Copies never hardlink host files and refuse symlinks/special files. Limits are 100,000 entries, depth 128, 4,096-byte paths, 256 MiB per file and 4 GiB total. Missing dependencies fail offline; no implicit fetch is performed.

The runtime strips inherited `CARGO_*`, `RUST*` and `LD_*` overrides and installs explicit compiler and private target settings. Its working directory is outside the candidate, with no Cargo configuration in its ancestors or private Cargo home. Candidate `.cargo/config` files therefore do not select a compiler or target directory. Operator toolchain/cache ancestors must remain immutable while used. Identity checks reject stable replacement but do not prevent a concurrent same-UID mutation after a check. Candidate build scripts and proc macros remain executable code under the parent's existing Stage restrictions; this is not compiler attestation.

For a governed product call, the policy command is a Forge program alias granted to the operator; the alias resolves to the installed Forge CLI. For example, the external-stage argv for formatting is:

```toml
command = ["forge", "candidate", "cargo-lint", "--mode", "fmt", "--execute", "--", "{files}"]
version_command = ["forge", "candidate", "cargo-lint", "--mode", "fmt", "--tool-version"]
cache = false
```

Here `forge` is an explicit grant alias for the installed Forge executable. Stage supervision supplies the granted environment, readable toolchain/cache/helper paths, process limits and descendant cleanup. Invoking `forge candidate cargo-lint --execute` directly in a terminal does not provide that supervision. Ordinary nonzero tool exits do not stop subsequent planned manifests; the latest nonzero exit is retained. Missing prerequisites produce exit 2 if no tool failed. Spawn, signal and cleanup failures fail explicitly.

The Python `cargo_lint_files.py` wrapper, native ambient toolchain-recovery shim, and `python_contracts_candidate_cargo_lint` compatibility target are deleted. Candidate policy keeps the existing check IDs but routes command and version invocations to `forge candidate cargo-lint` with `cache = false`. The repository has 125 Python source files under `src/`; the broader native migration remains incomplete.

## Validation and measurements

Focused release validation on 2026-11-01 passed 25 core planner tests, eight runtime tests, 12 CLI tests, one real seeded-dependency test and three stage tests. Six toolchain/delegation-dependent tests are ignored by default and were explicitly executed with Rust 1.98.0 and a delegated Linux cgroup. They cover real rustfmt and Clippy success/failure, workspace exclusions, borrowed sources, staged versus dirty original bytes, actual version reporting, private cache copying, ignored candidate compiler configuration and cleanup. The repository's four production crates also passed the new native fmt command. These are test-fixture execution results, not a complete review receipt or hostile-compiler attestation.

The release benchmark used an AMD Ryzen 9 9950X3D and Linux 7.0.0-38-generic. Five warmups preceded 31 samples of 20 calls each. Median sample means were 6.161 microseconds for parsing a 16-crate roster and 138.350 microseconds for planning 16 crates with filesystem enumeration, ownership and manifest reads; p95 sample means were 6.382 and 143.722 microseconds. The host was unisolated and a Forge release build ran concurrently. These current-only synthetic measurements include returned-object destruction and exclude fixture construction, toolchain/cache admission, compiler execution, sandboxing and CLI startup. They establish no speedup or end-to-end dispatch latency. All 1,240 timed calls completed within the cooperative deadline. [Raw measurements and provenance](../research/performance/native-cargo-lint-20261101.json) bind the workload, output, source and benchmark executable.

Reproduce the benchmark with:

```sh
CARGO_BUILD_JOBS=2 PYO3_PYTHON=/nonexistent/forbidden-python \
  cargo +1.98.0 bench --offline --locked \
  --manifest-path native/conductor-native/Cargo.toml \
  --features bundled-sqlite --bench candidate_cargo_lint
```
