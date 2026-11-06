//! Bounded cooperative observations from ordinary package-wide libtest output.
use super::{array, require, safe_token, summary_counts, test_section, MAX_LOG_BYTES};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const MAX_CASES: usize = 100_000;
const MAX_FRAMES: usize = 4096;

struct Frame {
    declared: usize,
    passed: usize,
    ignored: usize,
    cases: BTreeSet<String>,
}
impl Frame {
    fn new(line: &str) -> Result<Self, String> {
        let rest = line
            .strip_prefix("running ")
            .ok_or("invalid libtest header")?;
        let (number, noun) = rest.split_once(' ').ok_or("invalid libtest header")?;
        let declared = number
            .parse::<usize>()
            .map_err(|_| "invalid libtest count")?;
        require(
            number == declared.to_string() && noun == if declared == 1 { "test" } else { "tests" },
            "invalid libtest header",
        )?;
        require(declared <= MAX_CASES, "libtest count exceeds bound")?;
        Ok(Self {
            declared,
            passed: 0,
            ignored: 0,
            cases: BTreeSet::new(),
        })
    }
    fn case(&mut self, line: &str) -> Result<(), String> {
        let case = line
            .strip_prefix("test ")
            .ok_or("unexpected libtest case line")?;
        let (name, status) = case.rsplit_once(" ... ").ok_or("malformed libtest case")?;
        require(
            !name.is_empty() && name.len() <= 4096 && !name.chars().any(char::is_control),
            "invalid libtest case name",
        )?;
        require(self.cases.insert(name.to_owned()), "duplicate libtest case")?;
        match status {
            "ok" => self.passed += 1,
            value
                if value == "ignored"
                    || value.strip_prefix("ignored, ").is_some_and(|reason| {
                        !reason.is_empty() && !reason.chars().any(char::is_control)
                    }) =>
            {
                self.ignored += 1
            }
            _ => return Err("unsupported libtest case outcome".into()),
        }
        require(self.cases.len() <= self.declared, "too many libtest cases")
    }
    fn finish(self, line: &str) -> Result<(usize, usize), String> {
        require(
            self.cases.len() == self.declared,
            "incomplete libtest frame",
        )?;
        summary_counts(line, self.passed, 0, self.ignored)?;
        Ok((self.passed, self.ignored))
    }
}

/// Parse complete libtest frames. Output is observed text, not producer attestation.
pub fn parse_package_baseline(stdout: &str) -> Result<Value, String> {
    parse_frames(stdout, false)
}

fn parse_frames(stdout: &str, diagnostics: bool) -> Result<Value, String> {
    require(
        stdout.len() <= MAX_LOG_BYTES && !stdout.contains(['\0', '\r']),
        "baseline output exceeds bounds/format",
    )?;
    let mut current: Option<Frame> = None;
    let mut frames = 0usize;
    let mut passed = 0usize;
    let mut ignored = 0usize;
    let mut total = 0usize;
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        if line.starts_with("running ") {
            require(current.is_none(), "unmatched libtest frame")?;
            frames = frames.checked_add(1).ok_or("frame count overflow")?;
            require(frames <= MAX_FRAMES, "too many libtest frames")?;
            current = Some(Frame::new(line)?);
        } else if line.starts_with("test result: ") {
            let frame = current.take().ok_or("unmatched libtest summary")?;
            let (ok, skip) = frame.finish(line)?;
            passed = passed.checked_add(ok).ok_or("case count overflow")?;
            ignored = ignored.checked_add(skip).ok_or("case count overflow")?;
            total = total.checked_add(ok + skip).ok_or("case count overflow")?;
            require(total <= MAX_CASES, "libtest total exceeds bound")?;
        } else if let Some(frame) = current.as_mut() {
            frame.case(line)?;
        } else {
            require(
                diagnostics && !line.starts_with("test "),
                "unexpected output outside libtest frame",
            )?;
        }
    }
    require(current.is_none() && frames > 0, "incomplete libtest output")?;
    require(passed > 0, "no executed passing tests observed")?;
    Ok(json!({"frames":frames,"passed":passed,"ignored":ignored,
        "scope":"observed libtest output, not producer attestation"}))
}

fn package_phase(
    phase: &Value,
    cargo: &str,
    package: &str,
    version: &str,
    build: bool,
) -> Result<Vec<String>, String> {
    require(
        phase["phase"] == if build { "Build" } else { "Test" },
        "unexpected engine baseline phase",
    )?;
    require(
        phase["process_status"] == "Success",
        "engine baseline phase failed",
    )?;
    require(
        phase["duration"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.0),
        "invalid engine baseline duration",
    )?;
    let argv: Vec<String> = array(phase, "argv")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("invalid engine argv".to_owned())
        })
        .collect::<Result<_, _>>()?;
    let mut expected = vec![cargo.to_owned(), "test".into()];
    if build {
        expected.push("--no-run".into());
    }
    expected.extend([
        "--verbose".into(),
        format!("--package={package}@{version}"),
        "--locked".into(),
        "--offline".into(),
    ]);
    require(
        argv == expected,
        "engine baseline Cargo argv differs from admitted package",
    )?;
    Ok(argv)
}

/// Inspect one held engine baseline log; output is cooperative, not attestation.
pub fn parse_package_engine_baseline(
    outcome: &Value,
    log: &str,
    cargo: &str,
    package: &str,
    version: &str,
) -> Result<Value, String> {
    require(
        log.len() <= MAX_LOG_BYTES && !log.contains(['\0', '\r']),
        "engine baseline log exceeds bounds/format",
    )?;
    require(
        cargo.starts_with('/')
            && safe_token(cargo)
            && safe_token(package)
            && !package.contains('/')
            && safe_token(version)
            && !version.contains('/'),
        "invalid engine baseline identity",
    )?;
    require(
        outcome["scenario"] == "Baseline" && outcome["summary"] == "Success",
        "engine baseline scenario did not succeed",
    )?;
    let phases = array(outcome, "phase_results")?;
    require(
        phases.len() == 2,
        "engine baseline requires Build and Test phases",
    )?;
    let build = package_phase(&phases[0], cargo, package, version, true)?;
    let test = package_phase(&phases[1], cargo, package, version, false)?;
    parse_frames(test_section(log, &build, &test, false)?, true)
}

#[cfg(test)]
#[path = "mutation_cargo_package_observations_tests.rs"]
mod tests;
