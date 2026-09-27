# Portable guardrail audit

`python -m conductor.guardrail_audit --root PATH` audits the selected repository,
including a `src/` Python layout and Rust source. The default target is `.`;
generated directories such as `.venv`, `node_modules`, `target` and `.git` are
pruned before traversal. Symlinked directories and files are not followed.
All code gets file-size checks. Python additionally gets AST function-size,
complexity and hotspot checks; Rust/JS/C++ semantic checks are not claimed.

A host can narrow the scope in its own `pyproject.toml`:

```toml
[tool.conductor]
guardrail_targets = ["src", "native"]
```

`--targets path/one path/two` overrides that list. Paths must exist under the
selected root; missing, empty and escaping targets fail with exit 2.

Whole-tree Python duplication uses Pylint's similarity normalization and exact
pair comparator, with minimum similarity 10. Comments, docstrings, imports and
function signatures follow the selected host's Pylint configuration (ignored
by default). One bounded configuration-only Pylint subprocess resolves the same
settings as the former CLI, including `.pylintrc`, `pyproject.toml`, plugins and
initialization hooks; it never analyzes source files. The summary records the
effective normalization options. A Rust inverted index of normalized windows
selects potentially matching file pairs globally; there are no independent
batches that could hide a duplicate across directories. Pylint retains final
threshold and finding-group decisions. Hash collisions outside identical line
multisets are not treated as candidate matches. Source-local `pylint` disable
and enable directives are interpreted by Pylint's own scoped pragma handler.
The summary includes eligible
files, possible/candidate pairs, indexed windows and scan duration. Highly
repetitive trees can still require many real pair comparisons; the index does
not promise constant time for an inherently large matching output.

External checks use the selected eligible Python files. An incomplete check
produces a null JSON count and `n/a (tool did not complete)` in Markdown, an
`error`-severity infrastructure finding and exit 2, even without `--check`.
Completed zero findings remain zero. Scoped audits (`--staged-only` or
`--from-ref`) identify the external checks as not run; Python-free scopes mark
them not applicable. Code findings retain their own severity; `--check` exits
1 for critical/high code findings and 0 when their checks pass.

## Bounded validation, 2026-09-26

The indexed implementation and Pylint's original `Symilar` were compared in
the same process using distinct 30-line synthetic Python files. These inputs
exercise eliminating unrelated file pairs, not a worst-case repetitive tree:

| Files | Original total | Indexed total | Indexed pair candidates |
| --- | ---: | ---: | ---: |
| 100 | 0.237 s | 0.065 s | 0 / 4,950 |
| 400 | 3.701 s | 0.091 s | 0 / 79,800 |
| 1,600 | not run | 0.345 s | 0 / 1,279,200 |

A Forge production-source sample of 171 Python files produced two
findings in 3.063 seconds: 7 candidate pairs from 14,535 possible pairs and
35,964 normalized windows. No original whole-tree Pylint run was launched.
Correctness fixtures separately compare exact Pylint findings and locations
across directories, threshold boundaries, and ignored comments, docstrings,
imports and signatures. These measurements are local observations, not timing
thresholds asserted by the tests. Measurements preceded the configuration-only
subprocess added to preserve host normalization overrides; its overhead is not
included in those times.
