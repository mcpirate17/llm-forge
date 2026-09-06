"""Deterministic replay stubs for the equivalence probe.

The probe records a function's real arguments from its driver tests, then replays
them against the unmodified function and each ablation variant -- without the
driver tests' monkeypatches. For ``conductor.memory_index`` that meant a replayed
``main(["index", ...])`` embedded the whole live corpus through the broker and
wrote the real ``INDEX_PATH``: 180 s of module budget spent measuring nothing
(the probe-timeout advisory), plus a partial 350 MB index file left behind for
the next pytest run to trip over -- the ``query_index_file`` branch of ``main``
embeds before it loads, so a leftover file kills the targeted-tests run.

Seam objects are reachable two ways: module attributes (resolved through module
globals at call time) and captured default arguments (bound once at def time, as
``def query_index(..., embedder=embed_text)`` does). The overrides swap BOTH.
Only the replay sweep sees the overrides; the record phase (driver tests) is
untouched. Fakes are hash-derived and constant across the baseline and every
variant, so they cannot manufacture a difference; they exist purely so a replay
terminates.
"""

from __future__ import annotations

import contextlib
import hashlib
import pathlib
import tempfile
from collections.abc import Iterator
from typing import Any

from conductor.kb_retrieve import EmbeddingBatch

STUB_DIMENSION = 8

# Marker: the probe substitutes a throwaway directory path for this seam.
TMP_PATH = object()

# fingerprint/dimension/paid are exactly what `assert_embedding_meta` demands;
# `paid=True` skips the num_gpu/num_ctx shape attempts, which a stub cannot honor.
_STUB_METADATA: dict[str, Any] = {
    "fingerprint": "sha256:" + hashlib.sha256(b"probe-replay-stub").hexdigest(),
    "dimension": STUB_DIMENSION,
    "paid": True,
}


def _stub_vector(text: str) -> list[float]:
    digest = hashlib.sha256(text.encode("utf-8")).digest()
    return [digest[i] / 255.0 for i in range(STUB_DIMENSION)]


def _stub_embed_text(text: str, *, purpose: str = "document", **_: Any) -> list[float]:
    return _stub_vector(f"{purpose}\x00{text}")


def _stub_embed_batch(
    texts: list[str], *, purpose: str = "document", **_: Any
) -> EmbeddingBatch:
    return EmbeddingBatch(
        vectors=[_stub_vector(f"{purpose}\x00{text}") for text in texts],
        metadata=dict(_STUB_METADATA),
    )


# Keyed by module name. Only modules whose replay reaches process-external I/O
# are listed; adding one is the fix for a probe-timeout advisory on it.
REPLAY_STUBS: dict[str, dict[str, Any]] = {
    "conductor.memory_index": {
        "embed_text": _stub_embed_text,
        "embed_batch": _stub_embed_batch,
        "INDEX_PATH": TMP_PATH,
    },
}


@contextlib.contextmanager
def replay_stub_overrides(module: Any) -> Iterator[None]:
    """Apply the module's replay stubs; restore the originals on exit.

    A plain yield for modules with no entry, so unlisted modules probe exactly
    as they did before.
    """
    spec = REPLAY_STUBS.get(getattr(module, "__name__", ""))
    if not spec:
        yield
        return
    # Resolved replacements first: the defaults swap needs the SAME resolved
    # object the attribute gets, and the TMP_PATH marker must become a real path.
    resolved = {attr: _resolve(repl) for attr, repl in spec.items()}
    original_by_id = {
        id(getattr(module, attr)): resolved[attr]
        for attr in spec
        if hasattr(module, attr)
    }
    saved: list[tuple[Any, str, Any, bool]] = []
    try:
        for attr, value in resolved.items():
            existed = hasattr(module, attr)
            saved.append((module, attr, getattr(module, attr, None), existed))
            setattr(module, attr, value)
        for func, holder, original, rebuilt in _captured_default_swaps(
            module, original_by_id
        ):
            saved.append((func, holder, original, True))
            setattr(func, holder, rebuilt)
        yield
    finally:
        for holder, attr, original, existed in saved:
            if existed:
                setattr(holder, attr, original)
            else:
                delattr(holder, attr)


def _captured_default_swaps(
    module: Any, original_by_id: dict[int, Any]
) -> list[tuple[Any, str, Any, tuple | dict]]:
    """Swap function defaults referencing a stubbed seam object.

    ``__defaults__`` is a tuple, ``__kwdefaults__`` a dict; both are bound once,
    so a swap rebuilds them with the stub where the original seam object appeared.
    Returns (func, holder_name, original, rebuilt) rows; restore is setattr.
    """
    swaps = []
    for func in vars(module).values():
        if not callable(func) or getattr(func, "__module__", None) != module.__name__:
            continue
        for holder in ("__defaults__", "__kwdefaults__"):
            current = getattr(func, holder, None)
            if not current:
                continue
            if isinstance(current, tuple):
                values = list(current)
                changed = False
                for i, value in enumerate(values):
                    if id(value) in original_by_id:
                        values[i] = original_by_id[id(value)]
                        changed = True
                if changed:
                    swaps.append((func, holder, current, tuple(values)))
            else:
                rebuilt = dict(current)
                changed = False
                for key, value in current.items():
                    if id(value) in original_by_id:
                        rebuilt[key] = original_by_id[id(value)]
                        changed = True
                if changed:
                    swaps.append((func, holder, current, rebuilt))
    return swaps


def _resolve(replacement: Any) -> Any:
    if replacement is TMP_PATH:
        return pathlib.Path(tempfile.mkdtemp(prefix="eqprobe-stub-")) / "INDEX_PATH"
    return replacement
