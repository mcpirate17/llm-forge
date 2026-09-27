# Native runtimes at install time

The root `conductor-tooling` package depends on three local native distributions:
`conductor-native` and `slop-core` are Python extension wheels, and `forge-cli`
is a Maturin `bin` wheel that installs `forge` into the environment's scripts
directory. A fresh `uv sync --frozen` builds all three before any application
command runs. Rust and the SQLite development headers must be available for that
install; a compiler is not required when running the installed commands.

For this checkout, install into an isolated environment without writing the
checkout's shared `.venv`:

```sh
export UV_PROJECT_ENVIRONMENT=/tmp/forge-native-install-venv
export CARGO_BUILD_JOBS=2
export CUDA_VISIBLE_DEVICES=
uv sync --frozen --no-dev
FORGE_INSTALL_SMOKE_VENV="$UV_PROJECT_ENVIRONMENT" \
  cargo test --locked --jobs 2 --manifest-path native/forge/Cargo.toml \
  --test install_packaging -- --ignored --test-threads=1
```

The explicitly ignored Rust smoke case fails if the venv is missing. It runs
`$UV_PROJECT_ENVIRONMENT/bin/forge --version` and a real
`forge legacy-hook guard-check` JSON request, then imports both native Python
modules. Those child processes see only the venv's scripts directory on `PATH`,
invalid `CARGO`/`RUSTC` paths, and no inherited `PYTHONPATH` or `PYTHONHOME`.
Rust assertions verify the output and that both extension files came from that
venv. The regular Rust case checks the three root dependency/source declarations,
the Forge `bin` binding, and each extension's explicit Python/Cargo version
agreement. CI runs the ignored smoke case explicitly after `uv sync`; ordinary
`cargo test` still runs the manifest case without installing packages.

Maturin reads the Forge version dynamically from `native/forge/Cargo.toml`.
The extension wheel versions remain explicit in their `pyproject.toml` files
because the extension freshness check reads those strings; they must match
their Cargo versions. The Forge wheel omits `module-name` because it is a
binary, so extension freshness checks do not mistake it for an importable
Python module. The native pyprojects declare uv cache keys for Cargo metadata
and Rust sources. Forge's key also watches its `conductor-native` path
dependency, embedded routing and calibration data, and the Git commit stamped
into the executable. For a deliberately fresh rebuild after changing native sources,
use `uv sync --reinstall-package conductor-native --reinstall-package slop-core
--reinstall-package forge-cli`.

A downstream uv project needs all four source pins for a local or Git source
install: `conductor-tooling`, `conductor-native`, `slop-core`, and `forge-cli`.
uv source overrides in this repository do not propagate to the downstream
project. The current local-source example is in the [README](../README.md).

## Developer installation

`make install` performs the runtime sync and precompiles all three crates' test
targets, including the Python compatibility contracts and their child binaries.
It also compiles the standalone task worker and nested dispatcher fixture. The
dispatcher keeps its own Cargo dependency graph so its arbitrary-precision JSON
feature cannot change the production CLI's JSON behavior.

Installation runs sequentially with two Cargo workers and GPU visibility masked,
even under `make -j`. `UV_PROJECT_ENVIRONMENT` selects an isolated venv;
`CARGO_TARGET_DIR` selects an alternate Cargo output directory. Optional package
extras can be passed as `INSTALL_SYNC_ARGS='--extra mutation --extra graph'`.
`make native` deliberately rebuilds all three runtimes and repeats the prebuild.

Tests reuse fixture binaries only when their source, manifest/lockfile where
applicable, executable permissions, and binary hashes match. Missing or stale
fixtures use the existing local compilation path for standalone `cargo test`.
Tests whose purpose is compilation still compile their temporary inputs.

## Validation

On 2026-09-27, a source snapshot installed all three native runtimes into a fresh
isolated venv: conductor-native 0.1.62, slop-core 0.1.11, and forge-cli 0.8.2.
Building the four local packages took 3m 25s with two Cargo workers; this includes
the editable Python package and is an installation measurement, not a runtime
speedup claim. The Rust installation smoke passed with compilers unavailable to
its child processes.

A subsequent unchanged `uv sync --dry-run` reported no changes. Touching the
embedded routing policy or calibration data selected a Forge CLI reinstall;
touching conductor-native source selected both conductor-native and Forge CLI.
These were isolated cache-selection checks; they did not modify checkout source.

The isolated `make install` also passed, including all Rust test targets and
reusable fixtures. Direct execution of the dispatcher JSON-contract case and
task GPU-masking case reused the prebuilt binaries. Their `strace` execution
records contained no Cargo or rustc launches.
