# Native graph context

`forge graph` retrieves focused context from a host's existing
`.code-review-graph/graph.db`. It runs in Rust and does not start an interpreter,
embedding model, indexer, or background service.

```sh
forge graph --host /path/to/project context src/module.rs
forge graph --host /path/to/project context src/module.rs --symbol process
forge graph --host /path/to/project refs --text 'Check src/module.rs:42 before editing'
forge graph --host /path/to/project refs --body-file message.txt
```

Commands return JSON. `context` retrieves caller and callee relationships;
`refs` extracts repository file references and retrieves bounded supporting
context. Use the command's `--help` to inspect its input and output limits.
Both commands accept `--max-bytes` for the JSON output budget. Reference scans
deduplicate paths before reading source, cap the number of files, and report
`input_truncated` when a body-file prefix is used. Omission counts ending in
`_at_least` are lower bounds; the tool does not scan the entire index to count
everything it leaves out.

An absent, incompatible or detectably stale graph is reported explicitly. Source
hash mismatches suppress indexed relationships. Each projection reads a consistent
database snapshot. The graph remains owned and refreshed by the host's existing
code-review-graph installation.

The command follows the practical pattern of filtering data inside tools before
returning it to the agent, described in Anthropic's
[code execution with MCP](https://www.anthropic.com/engineering/code-execution-with-mcp).
It uses the SQLite index already present in the host. A future MCP adapter can
use the [official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk);
an additional parser or MCP service is not required for these CLI commands.
