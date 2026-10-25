//! The equivalence probe's gate: one `conductor.equivalence_probe` child per target
//! module, and what its findings mean.
//!
//! A target is a module and the units to ablate in it, each with the recorded tests that
//! ran its own lines (conductor-native's `equivalence_targets_native`). A module's
//! drivers are the union of its units'; when no unit has one, the test files that import
//! the module stand in, and every finding says which (`drivers_source`), `unrecorded`
//! for a unit no recorded test ran in a module others' records drive. A unit whose
//! drivers would overrun the timeout arrives refused and is incomplete. A child runs in
//! a private directory under `root` with a timeout, the machine's threads divided among
//! the children running at once. A child that times out, fails or writes no report, and
//! a unit with ablations its report says nothing about, are incomplete: nothing was
//! measured there, which a pass must not hide.

use crate::index::TestIndex;
use crate::{engine, index, rules};
use anyhow::{bail, Context};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const STDERR_TAIL_CHARS: usize = 2000;
const MAX_AUTO_JOBS: usize = 8;
const THREAD_VARS: [&str; 3] = ["OMP_NUM_THREADS", "MKL_NUM_THREADS", "OPENBLAS_NUM_THREADS"];

/// A unit to ablate: `(qualname, def line, driver nodeids)`.
pub type Unit = (String, usize, Vec<String>);
/// A unit no driver fit the timeout of: `(qualname, def line, why)`.
pub type Refused = (String, usize, String);
/// A target module: `(path, whole file, units, refused units)`.
pub type Target = (String, bool, Vec<Unit>, Vec<Refused>);

/// What a run measured: a whole module (`units` is `None`) or some of its units.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Measured {
    pub module: String,
    pub units: Option<Vec<String>>,
}

impl Measured {
    pub fn covers(&self, module: &str, qualname: &str) -> bool {
        self.module == module
            && self
                .units
                .as_ref()
                .is_none_or(|units| units.iter().any(|unit| unit == qualname))
    }
}

/// A run's findings by what they mean for the change.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Summary {
    pub modules_probed: usize,
    pub measured: Vec<Measured>,
    pub modules_without_drivers: Vec<String>,
    /// Modules no recorded test drove, probed with the test files importing them.
    pub modules_index_driven: Vec<String>,
    pub blocking: Vec<Value>,
    pub advisory: Vec<Value>,
    pub untested: Vec<Value>,
    pub unreached: Vec<Value>,
    pub incomplete: Vec<Value>,
    pub unmeasured: Vec<Value>,
    pub live: usize,
}

#[derive(Deserialize)]
struct Waiver {
    module: String,
    rule: String,
    #[serde(default = "every_function")]
    qualname: String,
}

fn every_function() -> String {
    "*".to_owned()
}

#[derive(Deserialize)]
struct Waivers {
    waivers: Vec<Waiver>,
}

impl Waiver {
    fn covers(&self, module: &str, finding: &Map<String, Value>) -> bool {
        self.module == module
            && finding.get("rule").and_then(Value::as_str) == Some(self.rule.as_str())
            && (self.qualname == "*"
                || finding.get("qualname").and_then(Value::as_str) == Some(self.qualname.as_str()))
    }
}

/// One module's probe.
struct Plan<'t> {
    module: &'t str,
    whole: bool,
    units: &'t [Unit],
    tests: Vec<String>,
    index_driven: bool,
}

/// A plan per target with units, the index built only for a module no recorded test
/// drove; a module neither names is recorded as without drivers, and a refused unit as
/// incomplete.
fn plans<'t>(
    root: &Path,
    targets: &'t [Target],
    index: &mut Option<TestIndex>,
    summary: &mut Summary,
) -> Vec<Plan<'t>> {
    let mut out = Vec::new();
    for (module, whole, units, refused) in targets {
        for (qualname, lineno, why) in refused {
            summary.incomplete.push(json!({
                "module": module,
                "qualname": qualname,
                "lineno": lineno,
                "verdict": "OVER_BUDGET",
                "rule": "drivers-over-budget",
                "description": why,
                "drivers_source": "record",
            }));
        }
        if units.is_empty() {
            continue;
        }
        let recorded: BTreeSet<&String> = units.iter().flat_map(|(_, _, tests)| tests).collect();
        // A module whose recorded drivers were refused is not one no recorded test drove.
        let index_driven = recorded.is_empty() && refused.is_empty();
        let tests = if index_driven {
            index
                .get_or_insert_with(|| index::build(root))
                .drivers_for(module)
        } else {
            recorded.into_iter().cloned().collect()
        };
        if tests.is_empty() {
            summary.modules_without_drivers.push(module.clone());
            continue;
        }
        if index_driven {
            summary.modules_index_driven.push(module.clone());
        }
        out.push(Plan {
            module,
            whole: *whole,
            units,
            tests,
            index_driven,
        });
    }
    out
}

/// What one child left: its report's findings, or why there is none.
enum Outcome {
    Findings(Vec<Value>),
    Incomplete(Value),
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let skip = text.chars().count().saturating_sub(STDERR_TAIL_CHARS);
    text.chars().skip(skip).collect()
}

fn incomplete(
    module: &str,
    rule: &str,
    description: String,
    took: Duration,
    stderr: &[u8],
) -> Value {
    let verdict = if rule == "timeout" {
        "TIMEOUT"
    } else {
        "PROBE_FAILED"
    };
    json!({
        "qualname": module,
        "rule": rule,
        "lineno": 0,
        "verdict": verdict,
        "description": description,
        "duration_s": (took.as_secs_f64() * 10.0).round() / 10.0,
        "stderr_tail": tail(stderr),
    })
}

/// The child's status, or `None` once it was killed at `timeout`.
fn wait(child: &mut Child, timeout: Duration) -> anyhow::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Run `plan`'s child in `workdir`. Its stderr goes to a file there, not a pipe, so a
/// grandchild that outlives a killed child cannot hold the gate open.
fn run(
    root: &Path,
    python: &str,
    plan: &Plan,
    workdir: &Path,
    timeout: Duration,
    threads: usize,
) -> anyhow::Result<Outcome> {
    let report = workdir.join("report.json");
    let log = workdir.join("stderr.log");
    let mut command = Command::new(python);
    command
        .args(["-m", "conductor.equivalence_probe", plan.module])
        .args(&plan.tests);
    for (qualname, _, _) in plan.units {
        command.args(["--function", qualname]);
    }
    command
        .arg("--json")
        .arg(&report)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(File::create(&log).with_context(|| log.display().to_string())?);
    for var in THREAD_VARS {
        if std::env::var_os(var).is_none() {
            command.env(var, threads.to_string());
        }
    }
    let started = Instant::now();
    let mut child = command
        .spawn()
        .with_context(|| format!("{python} -m conductor.equivalence_probe {}", plan.module))?;
    let status = wait(&mut child, timeout)?;
    let took = started.elapsed();
    let stderr = std::fs::read(&log).with_context(|| log.display().to_string())?;
    let module = plan.module;
    let Some(status) = status else {
        let description = format!(
            "probe exceeded its {}s budget; nothing was measured for this module",
            timeout.as_secs_f64()
        );
        return Ok(Outcome::Incomplete(incomplete(
            module,
            "timeout",
            description,
            took,
            &stderr,
        )));
    };
    if !status.success() {
        let code = status
            .code()
            .unwrap_or_else(|| -status.signal().unwrap_or(0));
        let description = format!("probe exited {code}; nothing was measured for this module");
        return Ok(Outcome::Incomplete(incomplete(
            module,
            "probe-exit",
            description,
            took,
            &stderr,
        )));
    }
    let text = match std::fs::read_to_string(&report) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let description =
                "probe exited 0 but wrote no report; nothing was measured for this module"
                    .to_owned();
            return Ok(Outcome::Incomplete(incomplete(
                module,
                "probe-no-report",
                description,
                took,
                &stderr,
            )));
        }
        Err(error) => return Err(error).with_context(|| report.display().to_string()),
    };
    Ok(match serde_json::from_str::<Vec<Value>>(&text) {
        Ok(findings) => Outcome::Findings(findings),
        Err(error) => {
            let description = format!(
                "probe report is not valid JSON ({error}); nothing was measured for this module"
            );
            Outcome::Incomplete(incomplete(
                module,
                "probe-bad-report",
                description,
                took,
                &stderr,
            ))
        }
    })
}

/// `run` in a private directory under `root` that is removed afterwards.
fn probe(
    root: &Path,
    python: &str,
    plan: &Plan,
    slot: usize,
    timeout: Duration,
    threads: usize,
) -> anyhow::Result<Outcome> {
    let workdir = root.join(format!(".equivalence-gate-{}-{slot}", std::process::id()));
    std::fs::create_dir(&workdir).with_context(|| workdir.display().to_string())?;
    let outcome = run(root, python, plan, &workdir, timeout, threads);
    std::fs::remove_dir_all(&workdir).with_context(|| workdir.display().to_string())?;
    outcome
}

/// Every plan's outcome, in plan order, from `jobs` children at a time: by default a
/// quarter of the CPUs, at most eight.
fn probe_all(
    root: &Path,
    python: &str,
    plans: &[Plan],
    timeout: Duration,
    jobs: Option<usize>,
) -> anyhow::Result<Vec<Outcome>> {
    if plans.is_empty() {
        return Ok(Vec::new());
    }
    let cpus = std::thread::available_parallelism().map_or(1, |count| count.get());
    let workers = match jobs {
        Some(0) => bail!("jobs must be at least 1"),
        Some(jobs) => jobs.min(plans.len()),
        None => (cpus / 4).clamp(1, MAX_AUTO_JOBS).min(plans.len()),
    };
    let threads = (cpus / workers).max(1);
    let next = AtomicUsize::new(0);
    let done: Mutex<Vec<Option<anyhow::Result<Outcome>>>> =
        Mutex::new((0..plans.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let at = next.fetch_add(1, Ordering::Relaxed);
                let Some(plan) = plans.get(at) else { break };
                let outcome = probe(root, python, plan, at, timeout, threads);
                done.lock().expect("a probe worker panicked")[at] = Some(outcome);
            });
        }
    });
    done.into_inner()
        .expect("a probe worker panicked")
        .into_iter()
        .map(|outcome| outcome.expect("every plan is probed"))
        .collect()
}

/// An incomplete finding per unit `findings` say nothing about that has ablations.
fn unreported(root: &Path, plan: &Plan, findings: &[Value]) -> anyhow::Result<Vec<Value>> {
    let reported: HashSet<&str> = findings
        .iter()
        .filter_map(|finding| finding.get("qualname").and_then(Value::as_str))
        .collect();
    let missing: Vec<&Unit> = plan
        .units
        .iter()
        .filter(|(qualname, _, _)| !reported.contains(qualname.as_str()))
        .collect();
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    let path = root.join(plan.module);
    let source = std::fs::read_to_string(&path).with_context(|| path.display().to_string())?;
    let mut ablations: HashMap<String, usize> = HashMap::new();
    for ablation in engine::collect(&source, rules::DEFAULT) {
        *ablations.entry(ablation.qualname).or_default() += 1;
    }
    Ok(missing
        .into_iter()
        .filter_map(|(qualname, line, _)| {
            let count = *ablations.get(qualname)?;
            Some(json!({
                "qualname": qualname,
                "rule": "probe-unit-unreported",
                "lineno": line,
                "verdict": "UNREPORTED",
                "description": format!(
                    "the probe reported nothing on {qualname}, which has {count} ablation(s); \
                     nothing was measured for this function"
                ),
            }))
        })
        .collect())
}

/// An index-driven `NOT_EXERCISED` that some test file names is a driver-selection
/// miss, not a coverage hole.
fn refine(finding: &mut Map<String, Value>, index: &TestIndex) {
    if finding.get("verdict").and_then(Value::as_str) != Some("NOT_EXERCISED") {
        return;
    }
    let Some(name) = finding
        .get("qualname")
        .and_then(Value::as_str)
        .and_then(|qualname| qualname.rsplit('.').next())
    else {
        return;
    };
    let named = index.named_by(name);
    if !named.is_empty() {
        finding.insert("verdict".to_owned(), json!("NOT_REACHED_BY_DRIVERS"));
        finding.insert(
            "named_by".to_owned(),
            json!(named.iter().take(4).collect::<Vec<_>>()),
        );
    }
}

fn file(summary: &mut Summary, finding: Map<String, Value>) -> anyhow::Result<()> {
    let verdict = finding
        .get("verdict")
        .and_then(Value::as_str)
        .context("a probe finding without a verdict")?;
    let bucket = match verdict {
        "REACHABLE_BUT_UNTESTED" => &mut summary.blocking,
        "NO_DIFFERENCE_OBSERVED" | "WITHIN_NUMERIC_NOISE" | "NONDETERMINISTIC" => {
            &mut summary.advisory
        }
        "NOT_EXERCISED" => &mut summary.untested,
        "NOT_REACHED_BY_DRIVERS" => &mut summary.unreached,
        "TIMEOUT" | "PROBE_FAILED" | "UNREPORTED" => &mut summary.incomplete,
        "OVER_BUDGET" | "BASELINE_UNUSABLE" | "UNCOMPILABLE" | "ARGUMENTS_UNCOPYABLE" => {
            &mut summary.unmeasured
        }
        "LIVE" => {
            summary.live += 1;
            return Ok(());
        }
        other => bail!("the probe reported an unknown verdict {other:?}"),
    };
    bucket.push(Value::Object(finding));
    Ok(())
}

fn summarize(
    root: &Path,
    plans: &[Plan],
    outcomes: Vec<Outcome>,
    index: Option<&TestIndex>,
    waivers: &[Waiver],
    summary: &mut Summary,
) -> anyhow::Result<()> {
    for (plan, outcome) in plans.iter().zip(outcomes) {
        let findings = match outcome {
            Outcome::Incomplete(finding) => vec![finding],
            Outcome::Findings(mut findings) => {
                summary.modules_probed += 1;
                summary.measured.push(Measured {
                    module: plan.module.to_owned(),
                    units: (!plan.whole).then(|| {
                        plan.units
                            .iter()
                            .map(|(qualname, _, _)| qualname.clone())
                            .collect()
                    }),
                });
                findings.extend(unreported(root, plan, &findings)?);
                findings
            }
        };
        let unrecorded: HashSet<&str> = plan
            .units
            .iter()
            .filter(|(_, _, tests)| tests.is_empty())
            .map(|(qualname, _, _)| qualname.as_str())
            .collect();
        for finding in findings {
            let Value::Object(mut finding) = finding else {
                bail!("{}: a probe finding that is not an object", plan.module);
            };
            let qualname = finding.get("qualname").and_then(Value::as_str);
            let source = if plan.index_driven {
                "index"
            } else if qualname.is_some_and(|name| unrecorded.contains(name)) {
                "unrecorded"
            } else {
                "record"
            };
            finding.insert("module".to_owned(), json!(plan.module));
            finding.insert("drivers_source".to_owned(), json!(source));
            if plan.index_driven {
                refine(
                    &mut finding,
                    index.context("an index-driven plan without an index")?,
                );
            }
            let blocking =
                finding.get("verdict").and_then(Value::as_str) == Some("REACHABLE_BUT_UNTESTED");
            if blocking
                && waivers
                    .iter()
                    .any(|waiver| waiver.covers(plan.module, &finding))
            {
                finding.insert("waived".to_owned(), json!(true));
                summary.advisory.push(Value::Object(finding));
                continue;
            }
            file(summary, finding)?;
        }
    }
    Ok(())
}

/// Probe `targets` under `root` with `python`, each module's child killed at `timeout`.
pub fn gate(
    root: &Path,
    python: &str,
    targets: &[Target],
    waivers: Option<&Path>,
    timeout: Duration,
    jobs: Option<usize>,
) -> anyhow::Result<Summary> {
    let waivers = match waivers {
        Some(path) => {
            let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
            serde_json::from_str::<Waivers>(&text)
                .with_context(|| path.display().to_string())?
                .waivers
        }
        None => Vec::new(),
    };
    let mut summary = Summary::default();
    let mut index = None;
    let plans = plans(root, targets, &mut index, &mut summary);
    let outcomes = probe_all(root, python, &plans, timeout, jobs)?;
    summarize(
        root,
        &plans,
        outcomes,
        index.as_ref(),
        &waivers,
        &mut summary,
    )?;
    Ok(summary)
}

/// The summary, as JSON, of probing `targets` (conductor-native's
/// `equivalence_targets_native`) under `root` with the interpreter `python`.
#[pyfunction]
#[pyo3(signature = (root, python, targets, waivers=None, timeout=180.0, jobs=None))]
pub fn equivalence_gate(
    py: Python<'_>,
    root: &str,
    python: &str,
    targets: Vec<Target>,
    waivers: Option<&str>,
    timeout: f64,
    jobs: Option<usize>,
) -> PyResult<String> {
    py.detach(|| {
        let summary = gate(
            Path::new(root),
            python,
            &targets,
            waivers.map(Path::new),
            Duration::from_secs_f64(timeout),
            jobs,
        )?;
        Ok(serde_json::to_string(&summary)?)
    })
    .map_err(|error: anyhow::Error| PyRuntimeError::new_err(format!("{error:#}")))
}
