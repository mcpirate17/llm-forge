# Governance identity and claim expiry test migration

Two shipped host-tool contracts now have Rust-owned PyO3 test drivers. The drivers call `conductor.candidate_review.identity`, `ownership`, and the candidate-review CLI; they do not import the retired Python tests or `conftest.py`. Forge's own repository has no governance-claim requirement. Every claim written by these tests is inside a temporary Git fixture.

| Original Python module | Rust target | Expanded cases |
| --- | --- | ---: |
| `src/conductor/test_governance_identity.py` | `python_contracts_governance_identity` | 19 |
| `src/conductor/test_ownership_claim_expiry.py` | `python_contracts_ownership_claim_expiry` | 19 |

The original baseline collected and passed **38 tests** with `/tmp/forge-native62-install-venv/bin/python -m pytest -q` and `CUDA_VISIBLE_DEVICES='' PYTHONDONTWRITEBYTECODE=1`: `38 passed in 0.35s`. `gh issue list --limit 100 --state open --json number,title,body,url` returned `[]`; no open issue covered this migration.

## Identity case correspondence

The Rust functions retain the Python test names, with one function for each expanded vendor parameter. They assert:

| Python case | Rust assertion |
| --- | --- |
| `a_worktree_is_named_for_itself_not_its_branch` | A `.git` file checkout resolves to its directory name despite a different branch. |
| `the_main_checkout_falls_back_to_its_branch` | A `.git` directory checkout resolves to normalized `codex/audit-rust-20260903`. |
| `a_detached_checkout_has_no_lane` | A detached `HEAD` yields the empty lane. |
| `the_vendor_pin_that_broke_codex_is_ignored` | Bare `GOVERNANCE_OWNER=codex` loses to the checkout lane. |
| `a_real_declaration_still_wins` | A concrete `GOVERNANCE_OWNER` declaration wins. |
| `the_vendor_only_stands_in_when_no_lane_can_be_derived` | `CODEX_HOME` selects `codex` only after lane derivation fails. |
| `an_unnameable_lane_raises_rather_than_guessing` | `OwnerIdentityError` says `no governance identity`. |
| `a_vendor_name_is_refused_as_a_claim_owner[claude]` | `is_vendor` is true; `require_lane_owner` raises with `names a vendor`. |
| `a_vendor_name_is_refused_as_a_claim_owner[codex]` | Same assertions for `codex`. |
| `a_vendor_name_is_refused_as_a_claim_owner[qwen]` | Same assertions for `qwen`. |
| `a_vendor_name_is_refused_as_a_claim_owner[grok]` | Same assertions for `grok`. |
| `a_vendor_name_is_refused_as_a_claim_owner[CoDeX]` | Same assertions for mixed case. |
| `a_vendor_name_is_refused_as_a_claim_owner[ codex ]` | Same assertions for padded vendor name. |
| `a_lane_name_is_accepted_and_folded` | `Codex/Rust Hotpath 20260903` folds to the exact expected slug. |
| `normalize_drops_what_the_owner_charset_cannot_hold` | Unsafe characters, all-punctuation input, and the 64-character cap match. |
| `the_legacy_vendor_of_a_lane_survives_a_bare_shell` | Prefix, launcher marker, and unknown prefix return `codex`, `claude`, and empty string respectively. |
| `a_claim_defaults_to_the_lane_that_will_write_it` | The child CLI succeeds and prints the exact lane owner. |
| `claiming_as_another_lane_is_refused` | The child CLI exits 1 and says `is not this lane`. |
| `claiming_as_a_bare_vendor_is_refused` | The child CLI exits 1 and says `names a vendor`. |

The original `_worktree` helper registered a Git worktree inside `tmp_path`. Forge's `AGENTS.md` forbids registering worktrees. The Rust fixture instead runs `git init --separate-git-dir` against a temporary checkout and temporary metadata directory, producing a real `.git` file and valid, distinct branch metadata. This preserves the `lane_of` inputs under test without registering a worktree. The three CLI children use the active PyO3 interpreter, explicit source `PYTHONPATH`, `GOVERNANCE_OWNER=''`, and the same `claim` options and `pkg/mod.py` target as the originals. Each `Case` clears ambient Git repository, index, object, namespace, and inline-config selectors before in-process Python calls; fixture and child Git commands also clear path selectors and use isolated global/system config. Git and ownership writes remain within `Case::root()`.

## Expiry case correspondence

The Rust functions retain all 19 Python test names. They call shipped Python APIs with the same keyword arguments, timestamps, and temporary repositories:

| Case | Preserved assertions |
| --- | --- |
| `create_refuses_a_max_above_the_ceiling` | `OwnershipError` and `claim max time must be`. |
| `create_accepts_a_max_at_the_ceiling` | Expiry minus creation equals the active cap. |
| `create_refuses_an_expected_beyond_its_own_max` | `OwnershipError` and `expected time must be`. |
| `create_stores_both_durations` | Exact 15m expected and 60m expiry deltas; expected precedes expiry. |
| `an_estimate_that_produced_no_writes_lapses_at_the_estimate` | Active at 14m, inactive at 16m, stored expiry beyond 59m. |
| `an_on_time_claim_keeps_the_long_idle_window` | No overrun and active just inside 45m; inactive just outside; lapse names creation. |
| `overrunning_shrinks_the_idle_window` | A write at 12m retains activity at estimate, then overrun and lapse after the short idle window while the long window remains open. |
| `a_busy_overrun_claim_still_holds` | Six successive writes keep the claim active through overrun, then it lapses after quiet. |
| `the_hard_cap_ends_even_a_busy_claim` | A legacy 8h store remains active one minute before the 2h cap, despite stored and idle deadlines beyond it, and reports hard expiry after it. |
| `the_lapse_message_names_the_overrun` | Reason names expected overrun and the short idle duration. |
| `a_claim_stored_without_an_expected_time_still_loads` | Legacy ID survives; absent expected field maps to the hard deadline and no early overrun. |
| `a_legacy_claim_round_trips_without_gaining_a_null_field` | Touch and a second claim preserve the legacy ID and omit `expected_at`, including after reload. |
| `a_legacy_overlong_claim_is_still_capped` | Stored lifetime remains 8h while active hard deadline is the 2h cap. |
| `a_write_resets_the_idle_timer` | Touch persists, activity equals the write time, and the idle timer moves. |
| `touching_twice_in_quick_succession_writes_once` | First touch persists, 30s touch debounces, sidecar retains first ISO stamp, 90s touch persists. |
| `a_lapsed_claim_stops_holding_the_path` | A stale owner's claim is replaced by the new owner; stale activity is pruned. |
| `a_live_claim_still_blocks_another_owner` | Overlapping live claim raises `OwnershipError`. |
| `activity_never_enters_the_claim_store` | `last_seen` stays out of the store, ID binding survives reload, sidecar exists. |
| `a_corrupt_activity_log_fails_loud` | Future activity schema raises `OwnershipError` with the version message. |

The legacy fixture calls production `sha256_json` over the original five fields with `paths` as a tuple. It writes the original pre-`expected_at` JSON shape to the temporary claim store; no replacement hashing or ownership algorithm was added. The short-lived repository and activity sidecar are deleted with `Case`.

## Source and discovery dependencies

Provider rows in the target registry are:

```text
src/conductor/candidate_review/identity.py\tpython_contracts_governance_identity
src/conductor/candidate_review/cli.py\tpython_contracts_governance_identity
src/conductor/candidate_review/ownership.py\tpython_contracts_governance_identity
src/conductor/candidate_review/git_source.py\tpython_contracts_governance_identity
src/conductor/candidate_review/model.py\tpython_contracts_governance_identity
src/conductor/project_paths.py\tpython_contracts_governance_identity
src/conductor/_native.py\tpython_contracts_governance_identity
native/conductor-native/src/lib.rs\tpython_contracts_governance_identity
src/conductor/candidate_review/ownership.py\tpython_contracts_ownership_claim_expiry
src/conductor/candidate_review/git_source.py\tpython_contracts_ownership_claim_expiry
src/conductor/candidate_review/model.py\tpython_contracts_ownership_claim_expiry
src/conductor/project_paths.py\tpython_contracts_ownership_claim_expiry
native/conductor-native/tests/python_contracts/support.rs\tpython_contracts_governance_identity
native/conductor-native/tests/python_contracts/support.rs\tpython_contracts_ownership_claim_expiry
```

`git_source.py` imports `project_paths.py`; the CLI imports its review engine and policy modules at startup. Both targets include the shared native `tests/python_contracts/support.rs` fixture and need `python-compat-tests`/PyO3; the CLI subprocess additionally needs the freshly built `conductor_native` extension on `PYTHONPATH`. Neither target calls `slop_core()`, so neither requires a candidate slop build. The originals were retired after independent parity review, registration, and native validation.

A source import search found no imports of either original Python test module in `src/` or `native/`; their only executable entrypoint was pytest collection. Historical mutation manifests and receipts are evidence records and are not runtime imports.

## Native verification

The integrated six-suite cohort passed all 99 cases after retirement against
the rebuilt 0.1.65 extension, plus all 28 discovery cases. Scoped Clippy with
warnings denied passed for all six targets and discovery. The integrated logs
are `/tmp/forge-native65-final-contracts.log`,
`/tmp/forge-native65-discovery-test.log`, and `/tmp/forge-native65-final-clippy.log`.

The exact two Rust targets passed with `cargo +1.98.0 test --offline --locked --manifest-path native/conductor-native/Cargo.toml --features python-compat-tests --test python_contracts_governance_identity --test python_contracts_ownership_claim_expiry -- --test-threads=1`: **19 + 19 passed**. The same exact targets passed `cargo +1.98.0 clippy --offline --locked --manifest-path native/conductor-native/Cargo.toml --features python-compat-tests --test python_contracts_governance_identity --test python_contracts_ownership_claim_expiry -- -D warnings`. Both runs used `/tmp/forge-native62-install-venv` for Python, the freshly built `/tmp/forge-contract-native64` extension, `SQLITE3_LIB_DIR=/tmp/forge-sqlite-link`, `CUDA_VISIBLE_DEVICES=''`, and two Cargo jobs. Logs: `/tmp/forge-native-identity-expiry-test.log` and `/tmp/forge-native-identity-expiry-clippy.log`.
