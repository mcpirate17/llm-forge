# Consolidation report shaping in Rust

`conductor.reuse.consolidation` keeps `FuncRecord`, `Cluster`, and its public
scan, report, and command functions. The existing `slop_core` operations still
own Python AST normalization and collection, duplicate grouping, evidence
scoring, batch assignment, and suggested-home selection. These algorithms
were already native and are not copied into `conductor-native`.

`conductor_native.reuse_consolidation_native` accepts JSON operations that run
without Python: `token_clones` shapes jscpd duplicate pairs into candidate
sites; `cluster_dicts` creates the published cluster rows; `report_summary`
counts dispositions, batches, redundant bytes, and value; `markdown_row` and
`markdown` render the report table. Python supplies suggested homes from the
existing native selector and the generated timestamp, then handles the jscpd
subprocess and file writes. The adapters retain the existing Python function
names and `Cluster` objects.

| Contract | Direct Rust test | Existing Python regression |
| --- | --- | --- |
| jscpd order, missing pairs, and lines fallback | `token_clone_ingest_preserves_order_and_lines_fallback` | `test_native_token_clone_evidence_matches_python` |
| Cluster row shape and summary scope | `output_rows_and_summary_keep_public_shape_and_actionable_scope` | `test_native_cluster_aggregation_matches_python`; audit inventory regressions |
| Markdown columns, rounding, and six-site cap | `markdown_preserves_table_text_float_rounding_and_six_site_cap` | `test_consolidation_native.py` and CLI report path |

The existing Python regressions remain active through the migration. No
historical audit report or clone evidence is rewritten.

Focused validation passed on 2026-09-27: three direct Rust contracts and 11
existing Python reuse regressions with the rebuilt `conductor-native` extension.
