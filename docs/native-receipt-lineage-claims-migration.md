# Receipt, lineage, and claims-view contract migration

These three shipped host-tool contracts are now Rust-owned PyO3 tests. They invoke production Python modules and their native bindings; they do not import `test_*.py` or `conftest.py`, and no replacement Python implementation was added. Original pytest baseline: **29 passed in 0.20s** with CUDA hidden and bytecode/cache writes disabled.

| Original module | Rust target | Expanded cases |
| --- | --- | ---: |
| `src/conductor/test_receipt_verify.py` | `python_contracts_receipt_verify` | 7 |
| `src/conductor/test_runner_lineage.py` | `python_contracts_runner_lineage` | 17 |
| `src/conductor/test_candidate_review_claims_view.py` | `python_contracts_candidate_review_claims_view` | 5 |

## Case correspondence

The Rust names match the Python names, except the lineage parameterized case has four explicit Rust functions.

| Original case | Preserved observation |
| --- | --- |
| `receipt_verify::all_four_parts_pass` | Human output returns 0, prints resolved repo/tree before `PASS`, names the actual tree, and reports two pins. |
| `receipt_verify::pass_with_raw_tree_oid` | JSON verdict returns 0, `PASS`, the raw tree OID, and `PASS` for exactly the four named checks. |
| `receipt_verify::json_resolution_lines_go_to_stderr` | JSON stdout parses as one object; tree resolution goes to stderr. |
| `receipt_verify::refused_on_empty_receipt` | A zero-byte receipt returns 4 and stderr contains both `REFUSED` and `receipt file is empty`. |
| `receipt_verify::refused_on_unresolvable_tree` | `deadbeef` returns 4 and emits `REFUSED`. |
| `receipt_verify::module_entrypoint_subprocess` | `python -m conductor.receipt_verify` returns 0, reports the expected tree OID, then `PASS`. |
| `receipt_verify::repo_defaults_to_cwd_discovery` | From the fixture's `repo/src`, omitted `--repo` resolves the temporary repo and expected tree. |
| `runner_lineage::accepts_an_exactly_recorded_hash_set` | The exact five-component set is accepted. |
| `runner_lineage::rejects_a_hash_set_that_was_never_recorded` | One unrecorded component hash is refused. |
| `runner_lineage::one_differing_component_is_still_a_miss` | Four of five matching components is refused. |
| `runner_lineage::a_subset_is_not_accepted` | A three-component subset is refused. |
| `runner_lineage::a_superset_is_not_accepted` | A sixth component is refused. |
| `runner_lineage::matches_any_entry_not_only_the_first` | A match in the second lineage entry succeeds after a nonmatching first entry. |
| `runner_lineage::absent_file_accepts_nothing` | Missing lineage file is refused. |
| `runner_lineage::malformed_json_accepts_nothing` | Malformed JSON is refused. |
| `runner_lineage::unknown_schema_version_accepts_nothing` | Schema 999 is refused. |
| `runner_lineage::entries_not_a_list_accepts_nothing` | Object-valued `entries` is refused. |
| `runner_lineage::empty_entries_accepts_nothing` | Empty entry list is refused. |
| `runner_lineage::non_dict_entry_is_skipped_not_fatal` | A string first entry is skipped and the second valid entry still matches. |
| `runner_lineage::a_non_mapping_recorded_value_is_refused[None]` | Rust `a_non_mapping_none_is_refused` sends Python `None` and expects false. |
| `runner_lineage::a_non_mapping_recorded_value_is_refused[string]` | Rust `a_non_mapping_string_is_refused` sends a string and expects false. |
| `runner_lineage::a_non_mapping_recorded_value_is_refused[42]` | Rust `a_non_mapping_number_is_refused` sends an integer and expects false. |
| `runner_lineage::a_non_mapping_recorded_value_is_refused[list]` | Rust `a_non_mapping_list_is_refused` sends a list and expects false. |
| `runner_lineage::shipped_lineage_is_wellformed_and_documented` | Present shipped file has schema 1, nonempty entries, each with justification, verification, diff, nonempty hashes of length 64, and nondecreasing component counts; absent file retains original skip behavior. |
| `claims_view::compact_view_is_one_line_per_claim_plus_paths` | Two active, zero expired/overrun; exactly three compact lines, truncated 72-character alpha reason, directory summaries, no full path until `--paths`, then five lines with both path lines. |
| `claims_view::path_filter_applies_to_both_views` | JSON selects alpha for `conductor/b.py`; compact views select one, then both, then none, and include selected full paths. |
| `claims_view::default_json_view_is_unchanged` | JSON has exactly `sha256` and `claims`, two claims, and the original seven stored fields. |
| `claims_view::compact_text_counts_expired_and_hides_them` | Fixed UTC time counts one active/one expired, hides dead ID, shows due/idle deadlines and on-time state. |
| `claims_view::compact_text_marks_an_overrun_claim` | Fixed UTC time shows one overrun and `idle   5/10m`. |

Receipt fixtures commit the source blobs and manifest to a temporary Git tree, while the receipt being authenticated remains outside that repo but inside the test case directory. The module child uses the active interpreter and the Forge `src` package root. Lineage fixtures are temporary JSON files; the shipped-lineage case reads only `src/conductor/mutation_runner_lineage.json`. Claims fixtures initialize a temporary Git repo with the same files and `create_claim` calls as the original fixture. They never read or write the live Forge or LLM claim store. Each Git-using `Case` clears ambient Git directory, worktree, index, object, namespace, ceiling, and inline config selectors before production calls, and Git subprocesses use isolated global/system config. `Case` restores process environment and `CwdRestore` restores cwd; no model, network, or GPU path is invoked.

## Source and discovery dependencies

Registered provider/helper rows:

```text
src/conductor/receipt_verify.py\tpython_contracts_receipt_verify
src/conductor/mutation_testing_support.py\tpython_contracts_receipt_verify
src/conductor/mutation_scope.py\tpython_contracts_receipt_verify
src/conductor/_native.py\tpython_contracts_receipt_verify
native/conductor-native/src/receipt_auth.rs\tpython_contracts_receipt_verify
native/conductor-native/src/lib.rs\tpython_contracts_receipt_verify
native/conductor-native/tests/python_contracts/support.rs\tpython_contracts_receipt_verify
src/conductor/mutation_testing.py\tpython_contracts_runner_lineage
src/conductor/mutation_campaign_model.py\tpython_contracts_runner_lineage
src/conductor/_native.py\tpython_contracts_runner_lineage
native/conductor-native/src/mutation_receipt.rs\tpython_contracts_runner_lineage
native/conductor-native/src/lib.rs\tpython_contracts_runner_lineage
native/conductor-native/tests/python_contracts/support.rs\tpython_contracts_runner_lineage
src/conductor/candidate_review/cli.py\tpython_contracts_candidate_review_claims_view
src/conductor/candidate_review/ownership.py\tpython_contracts_candidate_review_claims_view
src/conductor/candidate_review/git_source.py\tpython_contracts_candidate_review_claims_view
src/conductor/candidate_review/model.py\tpython_contracts_candidate_review_claims_view
src/conductor/project_paths.py\tpython_contracts_candidate_review_claims_view
src/conductor/_native.py\tpython_contracts_candidate_review_claims_view
native/conductor-native/src/lib.rs\tpython_contracts_candidate_review_claims_view
native/conductor-native/tests/python_contracts/support.rs\tpython_contracts_candidate_review_claims_view
```

`git_source.py` also imports `project_paths.py`, and the candidate-review CLI imports engine/check modules at load time. `receipt_verify.py` resolves Git blobs through `mutation_testing_support._git_bytes` and authenticates them through the compiled `receipt_auth.rs` functions. `mutation_testing._lineage_accepts` delegates through `mutation_campaign_model.py` and the compiled `mutation_receipt.rs` binding. The exact Rust targets require `python-compat-tests`/PyO3 and the freshly built `conductor_native` extension on `PYTHONPATH`; the receipt child also needs the active Python executable and Forge source path. A source import search found no executable imports of the three original test modules in `src/` or `native/`; their executable entrypoint was pytest collection. Historical manifests and receipts are evidence records, not runtime imports.

The originals were retired after independent parity review, target registration, and successful native validation.

## Native verification

After retirement, the integrated five-suite cohort passed all 64 cases against
the rebuilt 0.1.66 extension. All 28 discovery cases and 11 candidate-runner
cases also passed, including required candidate Slop builds. Scoped Clippy
with warnings denied passed for the five targets, discovery, and the runner.
Logs: `/tmp/forge-native66-final-contracts.log`,
`/tmp/forge-native66-discovery-test.log`, and `/tmp/forge-native66-final-clippy.log`.

The exact three Rust targets passed with `cargo +1.98.0 test --offline --locked --manifest-path native/conductor-native/Cargo.toml --features python-compat-tests --test python_contracts_receipt_verify --test python_contracts_runner_lineage --test python_contracts_candidate_review_claims_view -- --test-threads=1`: **7 + 17 + 5 passed**. The same exact targets passed `cargo +1.98.0 clippy --offline --locked --manifest-path native/conductor-native/Cargo.toml --features python-compat-tests --test python_contracts_receipt_verify --test python_contracts_runner_lineage --test python_contracts_candidate_review_claims_view -- -D warnings`. Both runs used `/tmp/forge-native62-install-venv` for Python, the freshly built `/tmp/forge-contract-native65` extension, `SQLITE3_LIB_DIR=/tmp/forge-sqlite-link`, `CUDA_VISIBLE_DEVICES=''`, and two Cargo jobs. Logs: `/tmp/forge-native-receipt-lineage-claims-test.log` and `/tmp/forge-native-receipt-lineage-claims-clippy.log`.
