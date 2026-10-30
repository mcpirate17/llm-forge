//! `forge sweep run|install`: the host's scheduled checks, run on a timer from
//! a persistent clone of the integration line, never inside a landing.
//!
//! A sweep checks out the target's head in its own clone (its own lock, so a
//! sweep never holds up a landing), builds it with `.forge/land.toml`'s env
//! and setup, then runs the land checks `.forge/sweep.toml` names -- with every
//! tracked path selected -- and the sweep's own `[[step]]`s. `FORGE_LAND_BASE`
//! is the head the last green sweep checked (`refs/forge/swept` in the clone),
//! so a range-scoped step covers exactly what landed since; the first sweep
//! covers the head commit alone. The ref advances only when every blocking
//! step passed, so a red range is checked again until it is fixed.
//!
//! Each run writes its step logs and `summary.md` under
//! `<report_dir>/<UTC stamp>/` in the invoking checkout and copies the summary
//! to `<report_dir>/latest.md` after every step, so a sweep the launcher's cap
//! kills leaves an INCOMPLETE verdict, not yesterday's; steps get the run directory as
//! `FORGE_SWEEP_OUT`. A sweep never commits or pushes. The target's copies of
//! both configs govern, as in `forge land`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::Deserialize;

use crate::instant::{format_compact_utc, isoformat_utc, now};
use crate::land::{
    acquire_lock, resolve_clone_dir, run_step, step_env, sync_clone, Config, Step, StepRun,
};
use crate::land_exec::{git, git_ok, tail, Outcome};

#[path = "sweep_unit.rs"]
mod unit;

const CONFIG_PATH: &str = ".forge/sweep.toml";
const SWEPT_REF: &str = "refs/forge/swept";
const WORK_BRANCH: &str = "forge-sweep";
const FAILURE_TAIL_LINES: usize = 40;

#[derive(Args)]
pub struct SweepArgs {
    #[command(subcommand)]
    action: SweepCommand,
}

#[derive(Subcommand)]
enum SweepCommand {
    /// Run the sweep now, in the foreground.
    Run,
    /// Write and enable a systemd user timer that runs the sweep on `calendar`.
    Install(unit::InstallArgs),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepConfig {
    /// The sweep's own persistent clone; never the land clone. Defaults to
    /// `<cache>/llm-forge/sweep/<repo>` under `$XDG_CACHE_HOME` (or `~/.cache`).
    #[serde(default)]
    pub clone_dir: Option<PathBuf>,
    /// Reports directory, relative to the invoking checkout.
    pub report_dir: PathBuf,
    /// Run directories kept under `report_dir`; older ones are deleted.
    #[serde(default = "default_keep_runs")]
    pub keep_runs: usize,
    /// systemd `OnCalendar=` expression the installed timer fires on.
    pub calendar: String,
    /// Argv prefix the timer runs `forge sweep run` under, such as a memory
    /// and wall cap; a relative program resolves against the checkout.
    #[serde(default)]
    pub launcher: Vec<String>,
    /// Land checks, by name, run with every tracked path selected.
    #[serde(default)]
    pub land_checks: Vec<String>,
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
}

fn default_keep_runs() -> usize {
    14
}

impl SweepConfig {
    /// Parses and validates against the land config whose checks it names.
    pub fn parse(text: &str, land: &Config) -> Result<Self> {
        let config: SweepConfig = toml::from_str(text).context("parsing sweep config")?;
        if config.keep_runs == 0 {
            bail!("keep_runs must be positive");
        }
        for step in &config.steps {
            step.validate()?;
            if !step.paths.is_empty() {
                bail!(
                    "sweep step {}: a sweep selects every path; drop `paths`",
                    step.name
                );
            }
        }
        let mut names: Vec<&str> = land.setup.iter().map(|s| s.name.as_str()).collect();
        for name in &config.land_checks {
            if !land.checks.iter().any(|c| &c.name == name) {
                bail!("land_checks names {name}, which .forge/land.toml does not define");
            }
            names.push(name);
        }
        names.extend(config.steps.iter().map(|s| s.name.as_str()));
        let mut sorted = names.clone();
        sorted.sort_unstable();
        if let Some(pair) = sorted.windows(2).find(|w| w[0] == w[1]) {
            bail!(
                "step name {} is used twice; each names its own log",
                pair[0]
            );
        }
        Ok(config)
    }

    /// Land checks, then the sweep's own steps, in run order.
    fn checks<'a>(&'a self, land: &'a Config) -> impl Iterator<Item = &'a Step> {
        let named = self.land_checks.iter().map(move |name| {
            land.checks
                .iter()
                .find(|c| &c.name == name)
                .expect("validated by parse")
        });
        named.chain(&self.steps)
    }
}

/// Reads both configs from a tree: the invoking checkout or the clone.
fn load(dir: &Path) -> Result<(Config, SweepConfig)> {
    let read = |rel: &str| {
        let path = dir.join(rel);
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    };
    let land = Config::parse(&read(crate::land::CONFIG_PATH)?)?;
    let sweep = SweepConfig::parse(&read(CONFIG_PATH)?, &land)?;
    Ok((land, sweep))
}

pub fn run(args: SweepArgs) -> Result<u8> {
    let here = std::env::current_dir()?;
    let (land, sweep) = load(&here)?;
    match args.action {
        SweepCommand::Run => sweep_now(&here, &land, &sweep),
        SweepCommand::Install(install) => unit::install(&here, &land, &sweep, &install),
    }
}

/// One summary row: a step that ran, or one a failed setup kept from running.
struct Row<'a> {
    step: &'a Step,
    setup: bool,
    run: Option<StepRun>,
}

impl Row<'_> {
    fn red(&self) -> bool {
        (self.setup || self.step.blocking) && !self.run.as_ref().is_some_and(StepRun::passed)
    }

    fn advisory_red(&self) -> bool {
        !self.setup && !self.step.blocking && self.run.as_ref().is_some_and(|run| !run.passed())
    }
}

/// The shared facts a summary reports beside its rows.
struct Header<'a> {
    target: &'a str,
    head: &'a str,
    base: &'a str,
    commits: &'a str,
    started: f64,
    secs: f64,
    /// Rows the finished run will have; fewer means it is still running or was cut short.
    total: usize,
    clone: &'a Path,
    out: &'a Path,
}

fn sweep_now(here: &Path, boot_land: &Config, boot: &SweepConfig) -> Result<u8> {
    let url = git(here, &["remote", "get-url", &boot_land.remote])?;
    let clone_dir = resolve_clone_dir(boot.clone_dir.as_deref(), "sweep", &url)?;
    let clone = clone_dir.as_path();
    let _lock = acquire_lock(clone)?;
    sync_clone(clone, &boot_land.remote, &url)?;
    let target = format!("{}/{}", boot_land.remote, boot_land.target);
    let head = git(
        clone,
        &["rev-parse", "--verify", &format!("{target}^{{commit}}")],
    )?;
    git(clone, &["checkout", "--quiet", "-B", WORK_BRANCH, &head])?;
    let (land, sweep) = load(clone)?;
    let base = sweep_base(clone, &head)?;
    let commits = git(clone, &["rev-list", "--count", &format!("{base}..{head}")])?;
    let started = now();
    let report_root = here.join(&sweep.report_dir);
    let out = report_root.join(format!("{}Z", format_compact_utc(started)));
    std::fs::create_dir_all(&report_root)?;
    // create_dir, not _all: a second run in the same second must not merge logs.
    std::fs::create_dir(&out).with_context(|| format!("creating {}", out.display()))?;
    let changed = clone.with_extension("changed");
    std::fs::write(&changed, git(clone, &["ls-files"])? + "\n")?;
    let mut env = step_env(clone, &land, &base, &changed)?;
    env.push(("FORGE_SWEEP_OUT".into(), out.to_string_lossy().to_string()));
    println!("sweep: {target} at {head}, {commits} commit(s) since {base}");
    let clock = Instant::now();
    let latest = report_root.join("latest.md");
    let total = land.setup.len() + sweep.checks(&land).count();
    let report = |rows: &[Row]| {
        let header = Header {
            target: &target,
            head: &head,
            base: &base,
            commits: &commits,
            started,
            secs: clock.elapsed().as_secs_f64(),
            total,
            clone,
            out: &out,
        };
        publish(&out, &latest, &render_summary(&header, rows))
    };
    let rows = run_rows(clone, &land, &sweep, &env, &out, &report)?;
    let red = rows.iter().filter(|row| row.red()).count();
    if red == 0 {
        git(clone, &["update-ref", SWEPT_REF, &head])?;
    }
    prune_runs(&report_root, sweep.keep_runs)?;
    println!(
        "sweep: {} ({} red); {}",
        verdict(red),
        red,
        latest.display()
    );
    Ok(u8::from(red > 0))
}

/// The last green sweep's head when the target still contains it; otherwise
/// the head's parent, so a first sweep covers the head commit alone.
fn sweep_base(clone: &Path, head: &str) -> Result<String> {
    let swept = format!("{SWEPT_REF}^{{commit}}");
    if git_ok(clone, &["rev-parse", "--verify", "--quiet", &swept]) {
        let sha = git(clone, &["rev-parse", &swept])?;
        if git_ok(clone, &["merge-base", "--is-ancestor", &sha, head]) {
            return Ok(sha);
        }
        eprintln!("sweep: {SWEPT_REF} {sha} is not in the target; covering the head commit alone");
    }
    let parent = format!("{head}^");
    if git_ok(clone, &["rev-parse", "--verify", "--quiet", &parent]) {
        return git(clone, &["rev-parse", &parent]);
    }
    // A root commit: nothing precedes it, so the range is empty.
    Ok(head.to_string())
}

/// Writes the run's summary, then swaps it into `latest.md` whole.
fn publish(out: &Path, latest: &Path, summary: &str) -> Result<()> {
    std::fs::write(out.join("summary.md"), summary)?;
    let staged = latest.with_file_name(".latest.md.tmp");
    std::fs::write(&staged, summary)?;
    std::fs::rename(&staged, latest)?;
    Ok(())
}

/// Setup in order (a failure stops the run), then every check, never stopping.
/// `report` sees the rows before the first step and after each one, so a sweep
/// cut short leaves a summary of what it finished.
fn run_rows<'a>(
    clone: &Path,
    land: &'a Config,
    sweep: &'a SweepConfig,
    env: &[(String, String)],
    out: &Path,
    report: &dyn Fn(&[Row]) -> Result<()>,
) -> Result<Vec<Row<'a>>> {
    let mut rows = Vec::new();
    let mut ready = true;
    report(&rows)?;
    for step in &land.setup {
        let run = if ready {
            Some(run_step(clone, step, env, out)?)
        } else {
            None
        };
        ready = run.as_ref().is_some_and(StepRun::passed);
        rows.push(Row {
            step,
            setup: true,
            run,
        });
        report(&rows)?;
    }
    for step in sweep.checks(land) {
        let run = if ready {
            Some(run_step(clone, step, env, out)?)
        } else {
            None
        };
        rows.push(Row {
            step,
            setup: false,
            run,
        });
        report(&rows)?;
    }
    if !ready {
        eprintln!("sweep: setup failed; no checks run");
    }
    Ok(rows)
}

fn verdict(red: usize) -> &'static str {
    if red == 0 {
        "GREEN"
    } else {
        "RED"
    }
}

fn render_summary(header: &Header, rows: &[Row]) -> String {
    let red = rows.iter().filter(|row| row.red()).count();
    let state = if rows.len() < header.total {
        format!(
            "**INCOMPLETE** ({} of {} step(s) finished; still running, or cut short)",
            rows.len(),
            header.total
        )
    } else {
        let advisory = rows.iter().filter(|row| row.advisory_red()).count();
        format!(
            "**{}** ({red} blocking step(s) red, {advisory} advisory red)",
            verdict(red)
        )
    };
    let short = |sha: &str| sha[..sha.len().min(10)].to_string();
    let mut text = format!(
        "# forge sweep: {} at {}\n\n\
         - verdict: {state}\n\
         - started: {}, wall {:.0} s\n\
         - range: {}..{} ({} commit(s))\n\
         - clone: {}\n\
         - logs: {}\n\n\
         | step | kind | result | seconds |\n|---|---|---|---|\n",
        header.target,
        short(header.head),
        isoformat_utc(header.started),
        header.secs,
        short(header.base),
        short(header.head),
        header.commits,
        header.clone.display(),
        header.out.display(),
    );
    for row in rows {
        let kind = match (row.setup, row.step.blocking) {
            (true, _) => "setup",
            (false, true) => "blocking",
            (false, false) => "advisory",
        };
        let (result, secs) = match &row.run {
            None => ("not run".to_string(), String::new()),
            Some(run) => (
                result_text(row.step, &run.outcome),
                format!("{:.1}", run.secs),
            ),
        };
        let _ = writeln!(text, "| {} | {kind} | {result} | {secs} |", row.step.name);
    }
    for row in rows {
        let Some(run) = row.run.as_ref().filter(|run| !run.passed()) else {
            continue;
        };
        let _ = write!(
            text,
            "\n## {}: {}\n\nlog: {}\n\n```\n{}\n```\n",
            row.step.name,
            result_text(row.step, &run.outcome),
            run.log.display(),
            tail(&run.log, FAILURE_TAIL_LINES).trim_end(),
        );
    }
    text
}

fn result_text(step: &Step, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Passed => "pass".into(),
        Outcome::Failed(code) => format!("FAIL exit {code}"),
        Outcome::TimedOut => format!("FAIL timed out after {} s", step.timeout_s),
    }
}

/// Deletes all but the newest `keep` run directories; touches nothing whose
/// name is not a sweep stamp.
fn prune_runs(root: &Path, keep: usize) -> Result<()> {
    let mut stamps: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        let named = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_stamp);
        if named && path.is_dir() {
            stamps.push(path);
        }
    }
    stamps.sort();
    let excess = stamps.len().saturating_sub(keep);
    for old in &stamps[..excess] {
        std::fs::remove_dir_all(old).with_context(|| format!("pruning {}", old.display()))?;
    }
    Ok(())
}

/// `YYYYMMDDTHHMMSSZ`, as `format_compact_utc` plus the zone letter.
fn is_stamp(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 16
        && bytes[8] == b'T'
        && bytes[15] == b'Z'
        && bytes[..8]
            .iter()
            .chain(&bytes[9..15])
            .all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAND: &str = r#"
target = "master"
clone_dir = "/tmp/land/x"

[[setup]]
name = "sync"
run = "true"
timeout_s = 60

[[check]]
name = "ruff"
run = "ruff check ."
paths = ['\.py$']
timeout_s = 300
"#;

    const SWEEP: &str = r#"
clone_dir = "/tmp/sweep/x"
report_dir = "reports/sweep"
calendar = "*-*-* 01:00:00"
land_checks = ["ruff"]

[[step]]
name = "audit"
run = "true"
timeout_s = 60
blocking = false
"#;

    fn land() -> Config {
        Config::parse(LAND).unwrap()
    }

    #[test]
    fn config_parses_and_orders_land_checks_before_sweep_steps() {
        let land = land();
        let sweep = SweepConfig::parse(SWEEP, &land).unwrap();
        assert_eq!(sweep.keep_runs, 14);
        let names: Vec<_> = sweep.checks(&land).map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["ruff", "audit"]);
    }

    #[test]
    fn config_rejects_unknown_keys_checks_paths_duplicates_and_zero_keep() {
        let land = land();
        let bad = [
            format!("{SWEEP}\nbogus = 1\n"),
            SWEEP.replace("[\"ruff\"]", "[\"nope\"]"),
            SWEEP.replace("blocking = false", "blocking = false\npaths = ['x']"),
            SWEEP.replace("name = \"audit\"", "name = \"sync\""),
            SWEEP.replace("name = \"audit\"", "name = \"ruff\""),
            format!("keep_runs = 0\n{SWEEP}"),
            SWEEP.replace("timeout_s = 60", "timeout_s = 0"),
        ];
        for text in bad {
            assert!(
                SweepConfig::parse(&text, &land).is_err(),
                "accepted:\n{text}"
            );
        }
    }

    fn step(name: &str, blocking: bool) -> Step {
        Step {
            name: name.into(),
            run: "true".into(),
            paths: Vec::new(),
            timeout_s: 9,
            blocking,
        }
    }

    fn ran(outcome: Outcome, log: &Path) -> Option<StepRun> {
        Some(StepRun {
            outcome,
            secs: 1.25,
            log: log.to_path_buf(),
        })
    }

    #[test]
    fn summary_counts_blocking_and_unrun_steps_red_and_tails_every_failure() {
        let dir = std::env::temp_dir().join(format!("forge-sweep-summary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("x.log");
        std::fs::write(&log, "first\nlast line\n").unwrap();
        let (setup, gate, debt, slow) = (
            step("sync", true),
            step("gate", true),
            step("debt", false),
            step("slow", true),
        );
        let rows = [
            Row {
                step: &setup,
                setup: true,
                run: ran(Outcome::Passed, &log),
            },
            Row {
                step: &gate,
                setup: false,
                run: ran(Outcome::Failed(3), &log),
            },
            Row {
                step: &debt,
                setup: false,
                run: ran(Outcome::Failed(1), &log),
            },
            Row {
                step: &slow,
                setup: false,
                run: ran(Outcome::TimedOut, &log),
            },
        ];
        let mut header = Header {
            target: "origin/master",
            head: "0123456789abcdef",
            base: "fedcba9876543210",
            commits: "4",
            started: 0.0,
            secs: 12.0,
            total: 5,
            clone: Path::new("/c"),
            out: &dir,
        };
        let partial = render_summary(&header, &rows);
        assert!(
            partial.contains(
                "verdict: **INCOMPLETE** (4 of 5 step(s) finished; still running, or cut short)"
            ),
            "{partial}"
        );
        header.total = 4;
        let text = render_summary(&header, &rows);
        assert!(
            text.contains("verdict: **RED** (2 blocking step(s) red, 1 advisory red)"),
            "{text}"
        );
        assert!(
            text.contains("range: fedcba9876..0123456789 (4 commit(s)"),
            "{text}"
        );
        assert!(text.contains("| sync | setup | pass | 1.2 |"), "{text}");
        assert!(
            text.contains("| debt | advisory | FAIL exit 1 | 1.2 |"),
            "{text}"
        );
        assert!(
            text.contains("| slow | blocking | FAIL timed out after 9 s |"),
            "{text}"
        );
        assert_eq!(text.matches("last line").count(), 3, "{text}");
        header.total = 1;
        let unrun = [Row {
            step: &gate,
            setup: false,
            run: None,
        }];
        assert!(render_summary(&header, &unrun).contains("| gate | blocking | not run |  |"));
        assert!(unrun[0].red());
        let advisory_only = [Row {
            step: &debt,
            setup: false,
            run: ran(Outcome::Failed(1), &log),
        }];
        assert!(render_summary(&header, &advisory_only)
            .contains("**GREEN** (0 blocking step(s) red, 1 advisory red)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prune_keeps_the_newest_runs_and_nothing_else_is_touched() {
        let root = std::env::temp_dir().join(format!("forge-sweep-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let names = [
            "20261001T010000Z",
            "20261002T010000Z",
            "20261003T010000Z",
            "notes",
            "2026100T010000Z",
        ];
        for name in names {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        std::fs::write(root.join("latest.md"), "x").unwrap();
        prune_runs(&root, 2).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "20261002T010000Z",
                "20261003T010000Z",
                "2026100T010000Z",
                "latest.md",
                "notes"
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
