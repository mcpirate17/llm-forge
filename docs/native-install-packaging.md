# Native runtimes at install time

`make install` builds the `forge-cli` Cargo package and installs the `forge`
executable under `target/install/bin`. Use `INSTALL_ROOT=/absolute/prefix` for
another destination and `CARGO_TARGET_DIR` for another compiler artifact directory.
The executable links the Forge, conductor-native and slop-core libraries without
Python bindings. SQLite is bundled. Rust 1.98.0 and at most two Cargo jobs are
selected explicitly; GPU visibility is masked.

```sh
make install INSTALL_ROOT="$PWD/target/install"
FORGE_TEST_BIN="$PWD/target/install/bin/forge" \
FORGE_INSTALL_ROOT="$PWD/target/install" \
  cargo +1.98.0 test --release --locked --jobs 2 \
  --manifest-path native/forge/Cargo.toml --test install_packaging \
  -- --include-ignored --test-threads=1
```

Adjust `FORGE_TEST_BIN` when selecting another installation prefix. The ordinary
smoke test relocates that executable. The explicitly ignored case requires
`FORGE_INSTALL_ROOT` and exercises its installed executable. Both cases run
with an empty `PATH`, invalid compiler paths, isolated home/configuration
locations and no source checkout. They cover version output, native hook guards,
AST analysis and provider configuration. Neither case proves Python compatibility.

`make test` installs the release CLI into a dedicated target-directory prefix
and runs default release tests for all native
libraries and both Forge workspace packages. CLI tests require the explicit
runtime `FORGE_TEST_BIN` handoff described in [local checks](local-checks.md).
Source-bound committed verification installs into `.git/forge-tools/native-install`
and runs the installed case explicitly. There is no hosted CI.

Tests reuse developer fixture binaries only when their source, manifest/lockfile
where applicable, executable permissions and binary hashes match. Missing or
stale fixtures compile on demand. Tests whose purpose is compilation still
compile their temporary inputs.

## Remaining compatibility distribution

While interpreted host adapters are being retired, the root Python package
still declares local `conductor-native`, `slop-core` and `forge-cli` distributions.
This compatibility installation is separate from the native build, install and
verification commands above. Its Python extension wheels are outside the native
gate's coverage.

The Forge compatibility distribution remains at `native/forge/pyproject.toml`.
Its Maturin configuration selects `../forge-cli/Cargo.toml`, reads version 0.8.9
from that package, and packages the `forge` executable as a `bin` wheel. It omits
`module-name`. Cache keys cover both Forge manifests, the CLI build script,
shared lockfile and sources, conductor-native and slop-core inputs, embedded
routing/calibration data and the stamped Git revision. Existing local source pins
remain at `native/forge`.

## Historical compatibility evidence

On 2026-09-27, a source snapshot installed conductor-native 0.1.62, slop-core
0.1.11 and forge-cli 0.8.2 into an isolated environment. Building four local
packages took 3m 25s with two Cargo workers, including the editable Python
package. The then-current compatibility smoke passed. Those results describe
the former installation workflow; they do not validate the current package split
or establish a native runtime speedup.
