# Candidate review mutation, hardening, and scan migration: 29 Python cases

Four Rust targets under `native/conductor-native/tests/` cover exactly the 29 Python cases below: `python_contracts_candidate_review_evidence.rs` (7), `python_contracts_candidate_review_waivers.rs` (4), `python_contracts_candidate_review_hardening.rs` (12), and `python_contracts_candidate_review_scan.rs` (6). Each imports the Rust-owned `python_contracts/candidate_review_support.rs` and `python_contracts/support.rs`; the shared fixture module also uses `python_contracts/git_fixture_support.rs`. Production Python modules are called through PyO3. No Python test module supplies fixtures.

| Python source/test (line) | Rust target/test | Oracle retained |
|---|---|---|
| `test_candidate_review.py:1564` javascript/native classification | evidence `javascript_and_native_specs_classify_as_tests` | JS spec and native test paths classify as `test`; production Python path does not. |
| `:1574` no tests changed | evidence `mutation_evidence_skips_when_no_tests_changed` | Mutation gate reports `skipped`. |
| `:1603` registry absent | evidence `mutation_evidence_fails_closed_without_registry` | Failed `mutation-registry-missing`. |
| `:1636` receipt absent | evidence `mutation_evidence_fails_closed_without_receipt` | Mocked verifier row produces failed `missing-mutation-receipt` at the changed test path. |
| `:1831` unavailable/malformed/value admission | evidence `unavailable_malformed_and_admitted_receipts` | `CampaignError` becomes unavailable; malformed rows fail; null value payload rejects; valid `CORE` value payload admits exact new nodeid. |
| `:2099` waiver policy validation | waivers `mutation_waiver_policy_accepts_bound_entry_and_rejects_malformed_variants` | Valid fields and source digest, 27 malformed single-field variants, duplicate IDs/paths/sources, missing owner. |
| `:2178` grandfather value gate | evidence `value_gate_anchors_grandfather_exemption` | Anchored old definition excluded; new definition alone has value finding and metric. |
| `:2252` inventory failure matrix | evidence `grandfather_inventory_failures_fail_closed` | Missing, label drift, whitespace-only byte drift, and milestone tampering produce `grandfather-inventory-invalid` and empty gated metrics. |
| `:2404` waiver runtime | waivers `mutation_waiver_runtime_conditions` | Applied waiver masks only receipt finding; value finding remains for waived and other tests; drift and wrong base deactivate waiver. |
| `:2468` candidate kinds | waivers `mutation_waiver_applies_across_candidate_kinds` | Index, range, commit all activate base-matched waiver. |
| `:2492` source binding | waivers `mutation_waiver_source_binding_requires_pinned_bytes` | Exact source digest activates; drift and disappearance deactivate. |
| `test_candidate_review_hardening.py:129` malformed containers | hardening `mutation_evidence_containers_fail_closed` | Both malformed containers and malformed row produce the expected rules and diagnostic messages. |
| `:155` malformed/duplicate rows | hardening `mutation_evidence_rows_fail_closed` | Malformed row numbers, duplicate row numbers/path, and unavailable value receipt remain distinct. |
| `:186` malformed payload metrics | hardening `mutation_metrics_survive_malformed_payload` | Failed status, zero coverage counts, malformed container and unavailable value rules. |
| `:212` anchor match | hardening `matching_grandfather_inventory_loads_from_anchor` | Two anchored labels, including class method, load exactly. |
| `:224` anchor commit absent | hardening `missing_anchor_commit_fails_closed` | `_GrandfatherError` says it cannot be proven from Git. |
| `:235` anchor tree drift | hardening `anchor_tree_drift_fails_closed` | `_GrandfatherError` reports tree drift. |
| `:248` crafted inventory | hardening `crafted_grandfather_inventory_fails_closed` | Updated digest cannot validate invented test definition. |
| `:267` shipped inventory | hardening `shipped_grandfather_inventory_has_expected_size` | 1,026 test files and 9,202 labels in shipped JSON. |
| `:281` dead path pruning | hardening `dead_inventory_entries_are_pruned` | Unlinked non-tombstoned path pruned; anchor proof still rejects tree drift. |
| `:306` verifier roots | hardening `verifier_receives_snapshot_and_anchor_repo` | `verify_evidence` receives snapshot as `repo_root` and host repo as `anchor_repo`. |
| `:336` tombstoned revival | hardening `tombstoned_lane_revival_stays_gated` | All 12 dead paths remain exempted from effective grandfather map even after one path is recreated. |
| `:409` value inventory corpus | hardening `value_inventory_covers_post_anchor_definitions_when_registered` | If ranked scope campaigns exist, ranked non-grandfathered nodeids equal live derived definitions, with no duplicate scope or ranking; otherwise this corpus-specific assertion returns, mirroring Python skip. |
| `test_candidate_review_scan_coverage.py:82` changed structure unreadable | scan `structure_audit_fails_closed_on_unreadable_changed_file` | High finding, one skipped file, exact path in skipped map. |
| `:98` unchanged structure unreadable | scan `unreadable_unchanged_file_stays_advisory` | Medium incomplete scan, no changed-file finding, one skip. |
| `:120` complete structure scan | scan `complete_scan_reports_coverage` | Zero skips, read/expected = 1, empty skipped map, no incomplete finding. |
| `:131` native unreadable | scan `native_source_unreadable_file_is_critical` | Critical changed-file finding, one skip. |
| `:151` native unsafe API | scan `native_source_still_flags_readable_unsafe_api` | Critical `unsafe-native-api`, one read, zero skips. |
| `:168` duplicate body unreadable | scan `duplicate_bodies_fail_closed_on_unreadable_changed_file` | High changed-file finding and one skip with changed-lines callback pinned to empty map. |

Provider closure before retiring Python tests:

| Current provider/dependent | Replacement or owner |
|---|---|
| Hardening Python imports main `_crafted_grandfather_inventory`, `_git`, `_probe_source`, `_write_grandfather_inventory` | The Rust hardening target uses `crafted_grandfather_inventory`, `git`, `probe_source`, and `write_grandfather_inventory` through shared support. |
| Scan Python imports main `_change` | The Rust scan target uses `added_change`, preserving status A and original mode/OID metadata. |
| `python_contracts_candidate_verification.rs:135` imports Python hardening `_gate_context` | Rewire to Rust `gate_context(py, case.root(), case.root(), &[], &"c".repeat(40))`, retaining the `Vec<AttrPatch>` through assertions; owned by candidate-imports rewire lane. |
| `python_contracts_candidate_policy_regressions.rs:1006` imports Python hardening `_gate_context` | Rewire `mutation_gate_fixture` to Rust `gate_context` with `[(PROBE, &["test_probe_legacy"])]`; return and retain `Vec<AttrPatch>`, remove `monkeypatch.undo`; owned by rewire lane. |
| `python_contracts_candidate_cli.rs:285` imports main Python `_receipt` | Rewire to shared Rust `fixture_receipt`; owned by rewire lane. |

The original Python candidate-review modules were retired after the replacement
cases, provider rewires, and benchmark repair passed focused checks. The four
Rust targets pass `rustfmt --edition 2021 --config skip_children=true --check`,
compile under Cargo 1.98.0 with `python-compat-tests`, pass all 29 focused
tests (7 + 4 + 12 + 6), and pass scoped Clippy with `-D warnings`.
