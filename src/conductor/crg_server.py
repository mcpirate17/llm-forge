#!/usr/bin/env python3
"""Start the pinned code-review-graph MCP with workspace embeddings."""

from __future__ import annotations

import argparse
import os
from pathlib import Path

from conductor.crg_embedding_bridge import CrgBridgeError, install_bridge
from conductor.crg_embedding_text import install_node_text
from conductor.crg_response_shim import (
    ResponseShimError,
    assert_supported_fastmcp,
    install_response_shim,
    prune_tools,
)
from conductor.crg_workspace_tools import register_workspace_tools, search_enrichers
from conductor.project_paths import host_root

ROOT = host_root()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=None)
    args = parser.parse_args(argv)
    root = Path(args.repo) if args.repo else ROOT
    try:
        install_bridge()
        install_node_text(root)
    except CrgBridgeError as exc:
        parser.error(str(exc))
    from code_review_graph.main import main as crg_main
    from code_review_graph.main import mcp

    # CRG_SHIM_DISABLE=1 is the harness A/B baseline (research/tools/codex_noshim.sh).
    if os.environ.get("CRG_SHIM_DISABLE", "").strip() != "1":
        register_workspace_tools(mcp)
        try:
            # Pin first: a fastmcp drift must report as a version mismatch, not
            # as a missing private registry inside prune_tools.
            assert_supported_fastmcp()
            prune_tools(mcp)
            install_response_shim(mcp, root, enrichers=search_enrichers(root))
        except ResponseShimError as exc:
            parser.error(str(exc))
    crg_main(repo_root=args.repo)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
