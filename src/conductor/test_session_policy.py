from __future__ import annotations

from dataclasses import FrozenInstanceError
from pathlib import Path

import pytest

from conductor.session_policy import (
    EMPTY_SESSION_POLICY,
    SessionPolicyError,
    load_session_policy,
)

ROOT = Path(__file__).resolve().parents[1]
EXPECTED_PREAMBLE = (
    "MISSION: Beat frontier models with novel non-QKV mechanisms. Never replace a novel lane with a softmax/QKV twin. Gate drops are defects.",
    'RETRIEVE (do not dump .current_work.md): `python -m conductor.kb_retrieve query "<task>" --top-k 5` then `python -m conductor.memory_index query "<task>" --top-k 8`. Code: code-review-graph MCP provider=openai model=qwen3-embed-cpu. Status: `python -m conductor.handoff append` (max 12 lines). Findings: research/notes/ then `memory_index index`.',
    "FLEET: embed http://127.0.0.1:7317/v1 (GPU-guest, num_ctx=2048, unload). Clerk qwen3.5:9b GPU-always, clerical-only, zero approval authority; never gate work or runs on local output. Do not load 27B. Paired probes: --compile-mode default (KB-HW-01).",
    "MUTATION: mutate ONLY the files you changed and their tests -- never repo-wide. `make mutation-plan` then `make mutation-generate` then `make mutation-engine-run`; automatic engines only, hand-authored mutants/manifests/baselines/receipts are forbidden (KB-MUT-02). A missing receipt is debt in the PR body, not a blocker.",
    "DELEGATE: searches touching >3 files, bulk reads, and summaries go to a subagent; keep the session context for decisions. Prefer ast_context_tool/query_graph over whole-file Read (>=400 lines: slice).",
)
EXPECTED_MANDATES = (
    "NOVEL_MECHANISMS_ONLY: Never replace novel lane with softmax/QKV twins. Gate drops are defects to fix.",
    "GRAPH_GATE: Call code-review-graph MCP before any Edit/Write.",
    "EAGER_REQUIRED: Paired comparisons and loss-sensitive probes use --compile-mode default.",
    "CLAIM_REQUIRED: Create narrow claim before editing (make governance-claim).",
    'MEMORY_RETRIEVE: Do not dump .current_work.md into context and do not write research into it. Query `python -m conductor.memory_index query "<task>" --top-k 8` and `python -m conductor.kb_retrieve query "<task>" --top-k 5`. Status ≤12 lines via `python -m conductor.handoff append`. Findings: research/notes then `memory_index index`. Code: code-review-graph MCP.',
    "AVO_USER_GATED: Autonomous variation (AVO) loops are user-invoked only. When a task is a continuous-improvement goal (iterative metric optimization, variation/evolution loops), prompt Tim first — 'This is a continuous-improvement goal — invoke AVO?' — and wait for his answer before starting any loop.",
    "LOCAL_AI_CLERICAL_ONLY: Local models have zero approval authority. Use them only for notes, summaries, organization, or compaction. Never use local output to approve, authorize, sign off, promote, launch, resume, continue, or spend optimizer/GPU on work or runs. Only Tim or a runtime-verified frontier model may approve where policy permits; multi-hour training still requires Tim's explicit approval.",
)


def _write_config(root: Path, body: str) -> None:
    root.mkdir(parents=True, exist_ok=True)
    (root / "pyproject.toml").write_text(body, encoding="utf-8")


def test_root_policy_preserves_the_exact_legacy_text() -> None:
    policy = load_session_policy(ROOT)
    assert policy.preamble == EXPECTED_PREAMBLE
    assert policy.standing_mandates == EXPECTED_MANDATES
    with pytest.raises(FrozenInstanceError):
        policy.preamble = ()  # type: ignore[misc]


def test_missing_file_or_session_table_is_generic_empty(tmp_path: Path) -> None:
    assert load_session_policy(tmp_path) is EMPTY_SESSION_POLICY
    _write_config(tmp_path, "[tool.conductor]\n")
    assert load_session_policy(tmp_path) is EMPTY_SESSION_POLICY


def test_missing_repository_is_not_a_generic_policy(tmp_path: Path) -> None:
    with pytest.raises(SessionPolicyError, match="existing directory"):
        load_session_policy(tmp_path / "missing")


@pytest.mark.parametrize(
    "body",
    (
        "[tool",
        "[tool]\nconductor = []\n",
        "[tool.conductor]\nsession = []\n",
        "[tool.conductor.session]\npreamble = []\n",
        "[tool.conductor.session]\npreamble = []\nstanding_mandates = []\nextra = []\n",
        "[tool.conductor.session]\npreamble = [1]\nstanding_mandates = []\n",
        '[tool.conductor.session]\npreamble = []\nstanding_mandates = [""]\n',
        '[tool.conductor.session]\npreamble = [" "]\nstanding_mandates = []\n',
    ),
)
def test_present_policy_is_complete_and_strict(tmp_path: Path, body: str) -> None:
    _write_config(tmp_path, body)
    with pytest.raises(SessionPolicyError):
        load_session_policy(tmp_path)


def test_policy_reader_refuses_an_oversized_or_nonregular_config(
    tmp_path: Path,
) -> None:
    _write_config(tmp_path, "x" * (64 * 1024 + 1))
    with pytest.raises(SessionPolicyError, match="exceeds 64 KiB"):
        load_session_policy(tmp_path)
    (tmp_path / "pyproject.toml").unlink()
    (tmp_path / "pyproject.toml").mkdir()
    with pytest.raises(SessionPolicyError, match="regular file"):
        load_session_policy(tmp_path)
