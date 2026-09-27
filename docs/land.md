# `forge land`

Lands a branch on the integration line without a CI service.

```
forge land <branch> [--dry-run] [--config PATH]
```

1. Takes an exclusive flock on `<clone_dir>.lock`, so landings run one at a time.
2. Brings the persistent clone at `clone_dir` up to date. It is created on first use,
   then cleaned (`reset --hard`, `clean -fd`) and fetched. Ignored state is kept
   (`.venv`, cargo `target/`), so setup only redoes what changed.
3. Reads the target branch's own `.forge/land.toml`. The config in the current
   checkout (or `--config`) only bootstraps a target that has no config yet, so a
   branch cannot loosen its own gate.
4. Rebases the remote branch onto the target. It refuses a rebase that conflicts, an
   empty range, or a commit that lacks any key in `required_trailers`.
5. Runs every `[[setup]]` step, then each `[[check]]` whose `paths` regexes match a
   changed path; a check with no `paths` always runs. Each step:
   - runs `sh -c` in the clone, with `path_prepend` on `PATH` and `[env]` set;
   - gets `FORGE_LAND_BASE` (the target sha) and `FORGE_LAND_CHANGED` (a file listing
     the changed paths);
   - is killed with its whole process group after `timeout_s`;
   - writes its log to `<clone_dir>.logs/<name>.log`.

   A failed step with `blocking = false` is reported but does not stop the landing.
6. Pushes the rebased head as a fast-forward of the target. If someone landed in the
   meantime, the remote refuses the push; run `forge land` again. It then deletes the
   branch with a lease on the sha that was checked.

Exit codes: 0 landed (or dry run passed), 1 a blocking step failed, 2 refused before
the checks ran (config, rebase, trailers, push).

Forge carries no host policy. The host's `.forge/land.toml` declares everything:
checks, bounds, trailers. See this repository's own `.forge/land.toml`.
