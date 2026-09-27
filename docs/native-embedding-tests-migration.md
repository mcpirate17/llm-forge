# Embedding contract test migration

The two Python test modules below are retired after their cases moved to
Rust-owned PyO3 contract targets. The targets call the shipped Python APIs, but
all test fixtures, expected values, type checks, and assertions are in Rust.
Both targets use temporary files and mocked modules only; they do not connect
to an embedding server or load a GPU model. `src/conductor/conftest.py` stays
available to other Python tests.

## CRG embedding text

Target: `native/conductor-native/tests/python_contracts_crg_embedding_text.rs`

| Retired Python case in `src/conductor/test_crg_embedding_text.py` | Rust case | Preserved checks |
| --- | --- | --- |
| `test_docstrings_for_functions_methods_classes_and_decorators` | `docstrings_for_functions_methods_classes_and_decorators` | Function, decorated function, class, method with qualified parent, and undocumented method |
| `test_docstring_prefers_line_proximity_then_unique_name` | `docstring_prefers_line_proximity_then_unique_name` | Wrong line with unique name; absent name |
| `test_non_python_and_unreadable_files_yield_no_docstring` | `non_python_and_unreadable_files_yield_no_docstring` | Rust file, missing Python file, syntactically broken Python file |
| `test_node_text_is_relative_and_bounded` | `node_text_is_relative_and_bounded` | Repository-relative path, kind, params, return type and docstring; whitespace collapse; 600-character ellipsis bound |
| `test_docstring_cache_is_keyed_by_mtime_and_size` | `docstring_cache_is_keyed_by_mtime_and_size` | Changed file content invalidates cache; max size 64 |
| `test_install_replaces_pinned_node_to_text` | `install_replaces_pinned_node_to_text` | Mocked CRG modules; bridge check called once; installed callback produces relative node text |
| `test_first_line_boundary_returns_empty_for_whitespace_only_docstring` | `first_line_boundary_returns_empty_for_whitespace_only_docstring` | `None`, empty string, whitespace-only multiline string |

## Route and receipt contract

Target: `native/conductor-native/tests/python_contracts_embedding_contract.rs`

| Retired Python case in `src/conductor/test_embedding_contract.py` | Rust case | Preserved checks |
| --- | --- | --- |
| `test_resolve_enforces_required_fields_dimension_and_cost` | `resolve_enforces_required_fields_dimension_and_cost` | Required-field error; paid soft skips for missing fields, API key, and cost; invalid and zero dimensions; nonnumeric and negative cost errors |
| `test_resolve_dimension_boundary[0-True]` | `resolve_dimension_boundary_zero_raises` | Zero dimension raises the typed positive-dimension error |
| `test_resolve_dimension_boundary[1-False]` | `resolve_dimension_boundary_one_resolves` | Dimension one resolves |
| `test_by_id_maps_route_id_to_spec` | `by_id_maps_route_id_to_spec` | Local route ID mapping |
| `test_validate_endpoint_rejects_bad_scheme_and_non_https_paid` | `validate_endpoint_rejects_bad_scheme_and_non_https_paid` | Absolute HTTP(S) URL and paid HTTPS errors |
| `test_load_routing_config_rejects_malformed_variants` | `load_routing_config_rejects_malformed_variants` | Missing file, unsupported schema, missing policy, invalid route, duplicate IDs, missing referenced route |
| `test_load_quality_receipt_rejects_malformed_variants` | `load_quality_receipt_rejects_malformed_variants` | Invalid JSON, schema, routes, result type, status, fingerprint and FAIL at/above threshold |
| `test_optional_float_rejects_non_numeric_and_bool` | `optional_float_rejects_non_numeric_and_bool` | `None`, integer conversion and typed errors for string and boolean |
| `test_route_attempts_raises_when_fingerprint_matches_no_route` | `route_attempts_raises_when_fingerprint_matches_no_route` | Unmatched pinned fingerprint error |
| `test_default_route_is_local_and_provider_neutral` | `default_route_is_local_and_provider_neutral` | Single local route, protocol, unpaid status, fingerprint prefix and public metadata without API key |
| `test_quality_failure_selects_explicit_paid_fallback` | `quality_failure_selects_explicit_paid_fallback` | Failed local receipt selects only paid route with exact quality-fallback enum and paid flag |
| `test_paid_route_is_disabled_without_explicit_configuration` | `paid_route_is_disabled_without_explicit_configuration` | Failed local receipt and empty environment produce no-approved-route error |
| `test_pinned_index_never_switches_vector_spaces` | `pinned_index_never_switches_vector_spaces` | Matching local fingerprint stays on local route with exact pinned-index enum despite paid configuration |
| `test_rejects_non_loopback_local_endpoint` | `rejects_non_loopback_local_endpoint` | Non-loopback local URL rejected during attempt selection |
| `test_quality_receipt_cannot_claim_pass_below_threshold` | `quality_receipt_cannot_claim_pass_below_threshold` | PASS below threshold rejected |
| `test_quality_receipt_is_bound_to_exact_route_fingerprint` | `quality_receipt_is_bound_to_exact_route_fingerprint` | Mismatched quality fingerprint rejected even with a paid fallback |

There are 7 CRG and 16 route/receipt Rust tests: 23 total, corresponding to
22 named Python test functions and the extra parameter row. Each target passes
with `--features python-compat-tests`, the pinned offline Cargo toolchain, and
one test thread. Scoped clippy also passes with `-D warnings`.
