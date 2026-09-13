# Mutation platform: registering, refreshing and snapshotting campaigns

Engine contracts, ratchet semantics and the acceptance rule live in
`src/conductor/MUTATION_ENGINES.md`; the knowledge card for hand-authored
baselines is `docs/KB-MUT-02.md`. This page documents the platform behaviors
around them: how a campaign is registered, how one refreshes, and what the
disposable snapshot carries. All of it is automatic — agents run the
generator, the engine and the doctor; nothing here is hand-edited.

## Registering a campaign: one file per row in `registry.d/`

A campaign is registered by writing one file:

```
campaigns/registry.d/<campaign-id>.json
```

containing exactly one row:

```json
{
  "manifest": "campaigns/<campaign-id>.json"
}
```

`campaigns/registry.json` still exists but only as the envelope the loader
requires — `schema_version`, `enforcement`, the canonical `test_patterns`,
`receipt_directories` — with an empty `campaigns` array. Nothing appends to
that array again: it was the one file every lane wrote to, and every pair of
concurrent PRs conflicted on it (PRs #13, #16, #20 and #22 each needed a
merge-in for that alone). One file per row means two lanes registering in
parallel never touch the same file; the only conflict left is two branches
registering the *same* campaign id, which is exactly the case that should
conflict.

The reader (the native registry loader) merges the array and every fragment,
deduplicates by manifest path and yields the list **sorted by campaign id**,
so a given tree always resolves in the same order regardless of filesystem.

### Splitting a legacy array

`python -m conductor.mutation_registry_split` is the one-shot migration that
moves every row of a shared array into `registry.d/` fragments and empties
the array. It is idempotent — an already-split registry changes no byte —
and it refuses loudly *before writing anything* when two rows share a
campaign id, a row has no `manifest` string, or an existing fragment already
registers a different manifest. Half a split registry is worse than none.

## Refreshing a campaign admitted with extra tests

`python -m conductor.mutation_campaign_generate refresh <campaign>` is the
automatic path after a source or test change. A campaign whose source has no
test named after it (`test_<module>.py`) — one admitted with
`--extra-test SOURCE=TEST`, like `_bash_quiet.py` — refreshes like any
other: the recorded test list is carried forward unchanged and the
engine-recorded survivor baseline is never touched. `write --force` remains
the wrong answer for these (it resets the ratchet; that is how `bash_quiet`
once went 8 → 13 survivors); refresh refuses only when the declared source
no longer exists in the tree.

## What the disposable snapshot carries

Engine runs execute inside a snapshot of the working tree. Every file under
a `tests/`, `fixtures/` or `test_*` path is included **regardless of
suffix** — fixture trees keep `.db` payloads, `.txt` corpora and extension
less symlinks, and dropping them by suffix once sent green campaigns home as
`BASELINE_FAILED`. Everywhere else the source-suffix allowlist governs.

A host whose data files live outside those paths can extend the allowlist
once in `pyproject.toml`:

```toml
[tool.conductor]
snapshot_extra_suffixes = [".db", ".dat"]
```

## How a module pairs with its tests

The generator pairs a module with the `test_<name>.py` **beside it**, or
under a `tests/` directory that mirrors the module's own package path. A
sole same-basename test in no mirrored tree still pairs (the legacy layout
some older packages keep). Two same-basename candidates with no mirror are a
loud refusal naming both — guessing there once paired
`conductor/__main__.py` with tooling's `test___main__.py` from another
package, and every mutant came back unreached. A module with no candidate at
all is reported unpaired; that list is where to look before deleting code.
