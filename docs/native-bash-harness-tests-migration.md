# Bash and provider provisioning contracts in Rust

The one Bash guard case, two Bash quiet cases, and six named harness
provisioning cases move to `native/conductor-native/tests/` targets
`python_contracts_bash_guard_legacy.rs`, `python_contracts_bash_quiet_legacy.rs`,
and `python_contracts_harness_provisioning.rs`. The harness provider bootstrap
case has three parameter rows, so the cohort exercises eleven expanded cases.
The original pytest baseline passed 11/11 in
`/tmp/forge-bash-harness-baseline.log`.

The Rust targets call the production Python modules through PyO3. The quiet
target also runs the actual `post-bash-quiet.sh` entry point with JSON stdin;
Rust owns the spill directory, assertions, and expected output. The harness
target builds temporary projects and provider settings, runs `project_init`
against them, and checks idempotence, drift, interpreter commands, preserved
foreign hooks, and adapter import probing. Its subprocess probe and CRG
availability replacement are Rust callbacks bound to the original Python
signatures. No external messaging service is started.

The harness target includes dedicated
`tests/python_contracts/harness_provisioning_support.rs` and shared
`agent_comm_support.rs` and `support.rs`. Its direct providers include
`harness_provisioning.py`, `project_init.py`, `hook_installer.py`,
`project_paths.py`, and the dispatch registry. The hook installer and project
path calls pass through `conductor/_native.py` to native `hook_installer.rs`
and `project_paths.rs`. Historical mutation campaign references to the Python
test paths retain their original path and hash provenance; they are not active
imports or replacement coverage.

Pre-retirement Rust checks passed guard 1/1, quiet 2/2, and harness 6/6 in
`/tmp/forge-bash-harness-first-rust.log`. Scoped Clippy passed with warnings
denied in `/tmp/forge-bash-harness-first-clippy.log`.

Independent source-to-Rust parity review passed all eleven expanded cases.
The three original Python test modules were then retired. Post-retirement
checks passed guard 1/1, quiet 2/2, and harness 6/6 in
`/tmp/forge-bash-harness-post-retire-tests.log`; scoped Clippy passed with
warnings denied in `/tmp/forge-bash-harness-post-retire-clippy.log`.
