# Workspace runtime matrix native migration

`conductor.workspace_runtime_matrix` retains its public functions, dataclasses,
CLI flags, root resolution, and live call ordering. Python performs process,
hook, HTTP, model, file-write, and timestamp operations. The Rust
`conductor_native::workspace_runtime_matrix` module decides outcomes from
captured evidence. Its `dispatch` function is available to native callers
without Python; one PyO3 entry point exposes the same decisions to the Python
compatibility layer.

| Python entry point | Rust operation | Native-owned behavior |
| --- | --- | --- |
| `aggregate_status` | `aggregate_status` | Required-cell precedence |
| `extract_reported_tokens` | `extract_reported_tokens` | Cumulative JSONL usage parsing |
| `check_hook_configs` | `check_hook_configs` | Guard presence, JSON reading, file digests |
| `check_hook_programs` | `hook_program_cases`, `hook_program_verdict` | Hook fixture selection and execution verdict |
| `check_active_state` | `check_active_state` | Claim/cache freshness verdict |
| `check_launcher_programs` | `check_launchers` | Availability verdict and receipt |
| `check_embedding_canary` | `check_embedding` | Embedding policy and unload verdict |
| `check_retrievers` | `check_retrievers` | JSON result parsing and receipt |
| `load_graph_evidence` | `check_graph_evidence` | Semantic provenance verification and digest |
| `_gpu_compute_processes`, `clerk_gpu_preflight` | `parse_gpu_processes`, `gpu_preflight` | GPU process parsing and reservation rules |
| `_clerk_schema`, `_clerk_payload`, `_adjudicate_clerk_attempt` | `clerk_schema`, `clerk_payload`, `clerk_adjudicate` | Bounded clerk request and verdict |
| `reconcile_receipt`, `_reconcile_cell` | `replace_receipt_cells` | Cell substitution and status recomputation |

The native test module covers required status precedence, preserved launcher
usage, graph provider and trace constraints, receipt preservation, GPU
reservation, clerk unload and token bounds, and hook config guards. The
existing Python matrix tests remain the compatibility check for monkeypatch
seams and CLI behavior. Tests use temporary files and captured fixtures; they
do not invoke a model, training run, GPU process query, or live HTTP endpoint.

Validation passed: eight native unit tests, three Rust-owned Python API tests,
and the existing Python matrix regression suite. The API tests verify that a
blocked GPU preflight prevents any clerk HTTP request. The original Python
matrix tests remain until every remaining case has a passing Rust replacement.
