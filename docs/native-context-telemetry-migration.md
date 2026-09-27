# Native context telemetry aggregation

The deterministic aggregation and report path lives in
`native/conductor-native/src/context_telemetry_aggregate.rs`. It is a public Rust
module and builds with `--no-default-features`, without Python or PyO3. It
streams each JSONL source once, ignores malformed lines, preserves first-seen
order for equal totals, and computes both the legacy rows and rich report
sections from the same accepted events. A Rust caller supplies the clock
reading for `--since`; the core does not inspect environment state.

`src/conductor/context_telemetry.py` retains the public functions and CLI.
Its report wrapper supplies the current UTC time and a temporary filename
label, decodes the native JSON result, and deletes the temporary file.
Historically the rich report's `files` value held that deleted temporary
filename; preserving it avoids changing the public report shape. The wrapper
no longer copies filtered records into that file. Python continues to own
argparse, hook dispatch I/O, and exact event serialization.

The PyO3 boundary exposes `context_telemetry_summarize_native` for the legacy
summary and these additions: `context_telemetry_report_native`,
`context_telemetry_parse_since_native`, and
`context_telemetry_format_summary_native`. The first accepts paths, bound
bytes, optional duration, top count, UTC clock string, and report-file label.
The formatter accepts a report JSON string and a rich/basic flag. The
private Python `_parse_since` retains its `datetime` return type.

The native hook event has a separate variant with the strict content hash used
by Python's `hook_context_event`. The existing native binding keeps its
permissive byte-count behavior and unchanged result shape. The private Python
`_injected_context_text` remains callable through a native-backed wrapper.
Rust handles SHA-256 for the normal hook event in the same call that counts its
bytes. Rotation stays in Python: its sortable names, collision handling, and
mtime pruning differ from the older Rust rotation helper.

Validation is in
`native/conductor-native/tests/context_telemetry_aggregate.rs` (runs without
default features) and the existing PyO3 telemetry contract suite. The pure
tests cover malformed lines, stable ties, bound excess, time filtering,
session and hook totals, repeat counts, negative top slicing, missing paths,
human formatting, and duration errors. The existing Python contract suite
covers the public writer, summary, and report routes. The parent integration
lane owns the serial CPU build and test run for this shared checkout.

Forge's issue tracker had no open issue covering this migration when checked.
