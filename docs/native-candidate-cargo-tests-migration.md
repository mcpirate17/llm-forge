# Candidate cargo test migration

The 15 top-level cases from `test_cargo_audit_files.py` and 20 from
`test_cargo_lint_files.py` run in two Rust test targets:
`python_contracts_candidate_cargo_audit.rs` and
`python_contracts_candidate_cargo_lint.rs`. The audit blank-setting case has
two parameter values (`""` and absent/`None`), so these 35 Rust tests cover
36 original executions. Rust constructs the fixture trees, calls the existing
Python APIs through PyO3, and owns every contract assertion. `unittest.mock`
only supplies controlled external responses. No retired Python test is invoked.

## Audit cases

| Retired Python case | Rust test | Preserved contract |
| --- | --- | --- |
| `test_explicit_settings_are_never_overridden` | `explicit_settings_are_never_overridden` | Both pinned `RUSTUP_HOME` and `CARGO_HOME` beat a usable sandbox home. |
| `test_login_home_recovers_what_the_sandbox_home_hides` | `login_home_recovers_what_sandbox_home_hides` | Empty runtime home resolves both tool homes from the login account. |
| `test_sandbox_home_is_preferred_over_the_login_home` | `sandbox_home_precedes_login_home` | Usable sandbox rustup wins and differs from the usable login rustup. |
| `test_a_directory_without_its_marker_is_not_a_home` | `home_directories_require_their_own_markers` | Empty `.rustup` and `.cargo` directories lose to valid login homes. |
| `test_the_shell_home_rustup_leaves_behind_is_rejected` | `rustup_shell_without_default_is_rejected` | A settings file without a default toolchain and an empty toolchains directory lose to login rustup. |
| `test_a_named_toolchain_that_is_not_installed_is_rejected` | `named_but_uninstalled_toolchain_is_rejected` | A named default without an installed toolchain loses to login rustup. |
| `test_each_home_is_validated_by_its_own_marker` | `cargo_home_requires_bin_even_when_rustup_marker_exists` | Valid rustup is recovered; `.cargo/settings.toml` without `bin` never yields `CARGO_HOME`. |
| `test_a_host_without_rustup_is_left_alone` | `host_without_rustup_or_cargo_home_is_unchanged` | Neither missing home variable is invented. |
| `test_a_missing_home_variable_still_consults_the_login_home` | `missing_home_variable_uses_login_home` | An environment without `HOME` still recovers login rustup. |
| `test_a_blank_setting_is_not_a_setting[empty=""]` | `blank_and_absent_rustup_settings_both_use_login_home` | An exported empty `RUSTUP_HOME` resolves to login rustup. |
| `test_a_blank_setting_is_not_a_setting[empty=None]` | `blank_and_absent_rustup_settings_both_use_login_home` | An absent `RUSTUP_HOME` resolves to login rustup. |
| `test_login_home_survives_a_missing_passwd_entry` | `missing_passwd_entry_returns_no_login_home` | `pwd.getpwuid` raises `KeyError` for the current UID and `_login_home()` returns `None`. |
| `test_the_owning_lockfile_is_the_nearest_one_above_the_file` | `nearest_lockfile_above_changed_file_wins` | The crate lockfile wins over the root lockfile. |
| `test_a_path_outside_the_root_owns_no_lockfile` | `outside_root_owns_no_lockfile` | An outside Rust path has no owner despite a root lockfile. |
| `test_the_version_probe_runs_under_the_recovered_toolchain` | `version_probe_uses_recovered_toolchain_and_exact_command` | `main(["--version"])` returns zero; one `cargo audit --version` call uses `check=False` and the recovered rustup home. |
| `test_a_failed_version_probe_names_the_missing_rustup_home` | `failed_version_probe_reports_missing_rustup_home` | Return code 101 propagates and stderr names `export RUSTUP_HOME`; the probe receives exact argv and kwargs. |

## Lint cases

| Retired Python case | Rust test | Preserved contract |
| --- | --- | --- |
| `test_absent_roster_refuses_rather_than_linting_nothing` | `absent_roster_refuses` | Missing roster raises `RosterError`. |
| `test_malformed_roster_refuses` | `malformed_roster_refuses` | Malformed TOML raises `RosterError`. |
| `test_roster_without_manifest_globs_refuses` | `roster_without_manifest_globs_refuses` | A roster without manifest globs raises `RosterError`. |
| `test_crate_in_neither_tested_nor_excluded_is_reported` | `unclassified_crate_is_reported` | A new crate appears in `unclassified()`. |
| `test_fully_classified_tree_reports_nothing` | `fully_classified_tree_reports_nothing` | The complete fixture roster returns an empty list. |
| `test_prerequisite_blocks_only_while_the_artifact_is_absent` | `prerequisite_blocks_only_until_artifact_exists` | Missing archive returns a reason naming `make kernels`; creating it clears the block. |
| `test_crate_without_a_prerequisite_is_never_blocked` | `crate_without_prerequisite_is_never_blocked` | `a/skip` is never blocked. |
| `test_owning_crate_walks_up_to_the_manifest` | `owning_crate_walks_up_to_manifest` | A source file maps to `a/keep`. |
| `test_changed_crates_separates_orphans_from_owned` | `changed_crates_sorts_owners_and_keeps_orphans` | Input order yields sorted `a/keep`, `a/skip` and separate `loose.rs`. |
| `test_owning_crate_stops_at_root_and_never_claims_an_outer_manifest` | `owning_crate_respects_root_boundary` | Neither a loose in-root Rust file nor an outside path claims the parent manifest. |
| `test_owning_crate_returns_the_nearest_manifest_not_the_outermost` | `owning_crate_prefers_nearest_manifest` | Inner `a/keep` wins over root `Cargo.toml`. |
| `test_fmt_covers_every_crate_except_the_unstyled` | `fmt_covers_every_crate_except_unstyled` | `fmt` selects `a/keep`, `a/old`, excluding `a/skip`. |
| `test_clippy_reaches_only_the_linted_roster` | `clippy_reaches_only_linted_crates_when_prerequisite_is_met` | Once the archive exists, `clippy` selects only `a/keep`. |
| `test_clippy_skips_a_linted_crate_it_cannot_build` | `clippy_skips_linted_crate_with_unmet_prerequisite` | Permanently blocked `a/held` is not selected. |
| `test_main_refuses_when_a_crate_is_unclassified` | `main_refuses_unclassified_crate` | An instance-method fixture returning `a/new` makes `main` return 1. |
| `test_main_refuses_a_rust_file_owned_by_no_crate` | `main_refuses_rust_file_without_owning_crate` | `loose.rs` makes `main` return 1. |
| `test_main_passes_when_nothing_changed_maps_to_a_crate` | `main_passes_when_no_changed_file_maps_to_crate` | A fully classified bare tree with no changed files returns zero. |
| `test_default_roster_location_is_the_unconfigured_default` | `default_roster_location_matches_project_paths_default` | The unconfigured relative path, module `ROSTER`, and `DEFAULT_CRATE_ROSTER` agree. |
| `test_host_overrides_the_roster_location` | `host_can_override_roster_location` | Host configuration loads exactly `a/only` and resolves to the configured file. |
| `test_configured_but_absent_roster_fails_loud_naming_the_resolved_path` | `configured_missing_roster_error_names_resolved_path` | Missing configured roster raises `RosterError` naming its resolved path. |

Run both targets with `--features python-compat-tests --test
python_contracts_candidate_cargo_audit --test python_contracts_candidate_cargo_lint`
and `-- --test-threads=1`. `Case` serializes process environment and current
directory changes; the parent-manifest fixture remains inside its case tree.
