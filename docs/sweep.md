# `forge sweep`

Runs the host's scheduled checks on a timer: the whole-repository audits and the
range reviews that are too slow for every landing. No CI service is involved.

```
forge sweep run
forge sweep install [--print]
```

## `forge sweep run`

1. Takes an exclusive flock on `<clone_dir>.lock`. The sweep's `clone_dir` is its
   own, so a sweep never waits on a landing and a landing never waits on a sweep.
2. Brings the persistent clone up to date as `forge land` does, then checks out the
   target's head. The target's own `.forge/land.toml` and `.forge/sweep.toml` govern;
   the copies in the current checkout only locate the clone.
3. Picks the range base: the head of the last green sweep (`refs/forge/swept` in the
   clone) when the target still contains it, otherwise the head's parent, so a first
   sweep covers the head commit alone.
4. Runs every land `[[setup]]` step; a failed one stops the run. Then runs the land
   checks named in `land_checks`, followed by the sweep's own `[[step]]`s, and never
   stops early. Each step runs like a land step, with these differences:
   - `FORGE_LAND_BASE` is the range base;
   - `FORGE_LAND_CHANGED` lists every tracked path, so every land check runs in full;
   - `FORGE_SWEEP_OUT` is this run's directory, for a step's own JSON or Markdown.
5. Writes each step's log and a `summary.md` to `<report_dir>/<UTC stamp>/`. The
   `report_dir` is relative to the invoking checkout. The summary is rewritten, and
   copied to `<report_dir>/latest.md`, before the first step and after every step.
   Until the last step finishes its verdict reads **INCOMPLETE** (k of n steps
   finished), so a sweep that the launcher's cap or a reboot cuts short says so
   instead of leaving the previous run's verdict in place. A finished run reads
   **GREEN** or **RED**, with its blocking and advisory failure counts.
6. Moves `refs/forge/swept` to the head only when every setup and blocking step
   passed. A red range is therefore checked again by the next sweep, with whatever has
   landed since, until it is fixed. Advisory steps (`blocking = false`) are reported
   but never hold the ref.
7. Keeps the newest `keep_runs` run directories (default 14) and deletes the rest. It
   deletes only directories named like a stamp.

A sweep never commits or pushes. Exit codes: 0 green, 1 a setup or blocking step
failed, 2 refused before any step ran (config, clone, fetch).

## `forge sweep install`

Writes `forge-sweep-<checkout dir>.service` and `.timer` to
`$XDG_CONFIG_HOME/systemd/user` (default `~/.config/systemd/user`). It then runs
`systemctl --user daemon-reload` and `enable --now` on the timer. `--print` shows
both units and writes nothing.

- The service is a `Type=oneshot` run at `Nice=10`, in the checkout. It runs
  `launcher` followed by this forge binary's `sweep run`. A relative launcher program
  resolves against the checkout. systemd never starts a second sweep while one is
  active.
- The service records the installing shell's `PATH`. The user manager's own `PATH`
  usually lacks per-user toolchains such as nvm's node.
- The timer fires on `calendar`, a systemd `OnCalendar=` expression that
  `systemd-analyze calendar` validates before anything is written. It uses
  `Persistent=false`, so a fire missed while the machine was off is skipped.
- Install prints the sum of every step's `timeout_s`, the worst case for one run
  before the launcher's own cap.

Reinstall after changing `calendar`, `launcher` or the forge binary path.
`systemctl --user list-timers 'forge-sweep-*'` shows the next fire, and
`journalctl --user -u forge-sweep-<name>` shows the console output.

## `.forge/sweep.toml`

```toml
clone_dir = "/data/sweep/repo"      # never the land clone
report_dir = "reports/sweep"        # relative to the checkout
calendar = "*-*-* 00:30:00"
launcher = ["tools/run_capped.sh", "-m", "48G", "-t", "10800", "--"]
land_checks = ["cargo", "contracts"] # run with every path selected

[[step]]
name = "dependency-audit"
run = "pip-audit --progress-spinner off"
timeout_s = 600
blocking = false                    # advisory: reported, never holds the ref
```

Sweep steps take no `paths`. The checks a sweep names must exist in the land config.
Step names must be unique across the land setup, the named land checks and the sweep
steps, because each name is a log file.
