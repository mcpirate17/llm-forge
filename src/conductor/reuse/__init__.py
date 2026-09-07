"""Repo-wide reuse analysis: duplicate function clusters and near-duplicate files.

Ported from ``audit/orchestrator`` so the analyzers -- and the ``slop_core`` Rust
entry points behind them -- outlive the multi-model audit loop they were written for.

``core`` is resolved once, at import. There is no per-call fallback: a missing
extension raises here rather than silently degrading a repository-wide scan.
"""

from __future__ import annotations

from conductor._native import slop_core

core = slop_core()
