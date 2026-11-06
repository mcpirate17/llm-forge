//! Pure bounded observations from one original cargo-mutants 27.1.0 run.
//! Callers hold/revalidate input files, target inventory and executable identity.
//! Text output is cooperative evidence, not authentication against malicious tests.
use crate::mutation_value_inputs::cargo_identity;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_REPORT_BYTES: usize = 1024 * 1024;
pub const MAX_LOG_BYTES: usize = 1024 * 1024;
pub const MAX_TOTAL_LOG_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_MUTANTS: usize = 1000;
pub const MAX_RANKED: usize = 4096;
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Target {
    pub cargo_executable: String,
    pub package: String,
    pub version: String,
    pub test: String,
    pub test_path: String,
}
#[derive(Clone, Debug)]
pub struct ScenarioLog {
    pub path: String,
    pub text: String,
}
/// IDs come exclusively from Forge's existing cargo::rows/identify implementation.
/// The coordinator supplies the EXACT engine scenario associated with each ID.
#[derive(Clone, Debug, Serialize)]
pub struct ExpectedMutant {
    pub id: String,
    pub scenario: Value,
}
#[derive(Debug, Serialize)]
pub struct MutantObservation {
    pub id: String,
    pub scenario: Value,
    pub log_path: String,
    pub log_sha256: String,
    pub phases: Value,
    pub report: Value,
}
#[derive(Debug, Serialize)]
pub struct Observations {
    pub schema_version: &'static str,
    pub scope: &'static str,
    pub engine_baseline: Value,
    pub baseline_reports: Vec<Value>,
    pub mutants: Vec<MutantObservation>,
    pub mutant_reports: Value,
    pub mutant_outcomes: Value,
}
impl Observations {
    /// expected_spec MUST originate from the held operator declaration and validated IDs.
    /// This API creates no contracts, classifications, approval or execution authority.
    pub fn analyze(&self, expected_spec: Value) -> Result<Value, String> {
        let encoded = crate::mutation_value::analyze_reports(
            expected_spec,
            self.baseline_reports.clone(),
            &self.mutant_reports,
            &self.mutant_outcomes,
        )?;
        serde_json::from_str(&encoded).map_err(|error| error.to_string())
    }
}
fn bounded_json<T: Serialize + ?Sized>(value: &T, limit: usize) -> Result<usize, String> {
    struct Counter {
        count: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.count = self
                .count
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("JSON size overflow"))?;
            if self.count > self.limit {
                return Err(std::io::Error::other("JSON exceeds byte bound"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { count: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|e| e.to_string())?;
    Ok(counter.count)
}
fn require(ok: bool, message: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string {field}"))
}
fn array<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("missing array {field}"))
}
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}
fn safe_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 4096
        && token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"/_-.:@=+".contains(&c))
}
fn names(target: &Target, ranked: &[String]) -> Result<BTreeMap<String, String>, String> {
    require(
        target.cargo_executable.starts_with('/') && safe_token(&target.cargo_executable),
        "expected Cargo must be an absolute safe token",
    )?;
    require(
        [&target.package, &target.version, &target.test]
            .iter()
            .all(|v| safe_token(v) && !v.contains('/')),
        "invalid fixed Cargo target",
    )?;
    require(
        safe_path(&target.test_path) && target.test_path.ends_with(".rs"),
        "invalid target source path",
    )?;
    require(
        !ranked.is_empty() && ranked.len() <= MAX_RANKED,
        "ranked test count exceeds bounds",
    )?;
    let mut names = BTreeMap::new();
    for node in ranked {
        require(
            node.len() <= 4096
                && node
                    .split_once("::")
                    .is_some_and(|(path, _)| path == target.test_path),
            "ranked test is outside fixed target source",
        )?;
        require(
            names.insert(cargo_identity(node)?, node.clone()).is_none(),
            "duplicate ranked libtest identity",
        )?;
    }
    Ok(names)
}

/// Caller-supplied baselines are parsed observations, not proof of independence.
fn baseline(value: &Value, names: &BTreeMap<String, String>) -> Result<(), String> {
    require(
        value["status"] == "COMPLETE",
        "independent baseline is incomplete",
    )?;
    let tests = value["tests"]
        .as_object()
        .ok_or("baseline tests must be an object")?;
    require(
        tests.len() == names.len(),
        "baseline test inventory mismatch",
    )?;
    for node in names.values() {
        let row = &value["tests"][node];
        require(
            row["outcome"] == "PASSED" && row["cases"] == 1,
            "baseline case is not one passing observation",
        )?;
    }
    for key in [
        "missing_nodeids",
        "ambiguous_nodeids",
        "unmapped_cases",
        "unranked_failures",
    ] {
        require(
            array(value, key)?.is_empty(),
            "baseline contains missing/ambiguous/unranked cases",
        )?;
    }
    Ok(())
}
fn expected_argv(target: &Target, build: bool) -> Vec<String> {
    let mut args = vec![target.cargo_executable.clone(), "test".into()];
    if build {
        args.push("--no-run".into());
    }
    args.extend([
        "--verbose".into(),
        format!("--package={}@{}", target.package, target.version),
        "--locked".into(),
        "--offline".into(),
    ]);
    if !build {
        args.push(format!("--test={}", target.test));
    }
    args
}
fn phase_argv(phase: &Value, target: &Target, build: bool) -> Result<Vec<String>, String> {
    require(
        phase["phase"] == if build { "Build" } else { "Test" },
        "unexpected engine phase/order",
    )?;
    require(
        phase["duration"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.0),
        "invalid phase duration",
    )?;
    let argv: Vec<String> = array(phase, "argv")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("argv must contain strings".to_owned())
        })
        .collect::<Result<_, _>>()?;
    let expected = expected_argv(target, build);
    // Both documented explicit spellings are accepted; no other flags/filters are.
    let mut alternate = expected.clone();
    if !build {
        alternate.pop();
        alternate.extend(["--test".into(), target.test.clone()]);
    }
    require(
        argv == expected || argv == alternate,
        "actual Cargo phase argv differs from fixed target",
    )?;
    Ok(argv)
}
fn failure(status: &Value) -> bool {
    status
        .as_object()
        .is_some_and(|v| v.len() == 1 && v.get("Failure") == Some(&json!(101)))
}
fn phases(
    value: &Value,
    target: &Target,
    killed: bool,
) -> Result<(Vec<String>, Vec<String>), String> {
    let phases = array(value, "phase_results")?;
    require(
        phases.len() == 2,
        "scenario must have exactly Build and Test phases",
    )?;
    let build = phase_argv(&phases[0], target, true)?;
    let test = phase_argv(&phases[1], target, false)?;
    require(
        phases[0]["process_status"] == "Success",
        "scenario build did not succeed",
    )?;
    require(
        if killed {
            failure(&phases[1]["process_status"])
        } else {
            phases[1]["process_status"] == "Success"
        },
        "scenario has no ordinary expected test exit",
    )?;
    Ok((build, test))
}
fn test_section<'a>(
    log: &'a str,
    build: &[String],
    test: &[String],
    killed: bool,
) -> Result<&'a str, String> {
    let build_marker = format!("*** {}\n", build.join(" "));
    let test_marker = format!("*** {}\n", test.join(" "));
    require(
        log.matches(&build_marker).count() == 1 && log.matches(&test_marker).count() == 1,
        "scenario command markers missing/duplicated",
    )?;
    let (_, after_build) = log
        .split_once(&build_marker)
        .ok_or("missing build marker")?;
    let (build_body, after_test) = after_build
        .split_once(&test_marker)
        .ok_or("phase marker order mismatch")?;
    require(
        build_body
            .lines()
            .filter(|line| line.starts_with("*** "))
            .collect::<Vec<_>>()
            == vec!["*** result: Success"],
        "build log phase status/markers mismatch",
    )?;
    let result = if killed {
        "*** result: Failure(101)"
    } else {
        "*** result: Success"
    };
    require(
        after_test
            .lines()
            .filter(|line| line.starts_with("*** "))
            .collect::<Vec<_>>()
            == vec![result],
        "test log phase status/markers mismatch",
    )?;
    let (body, tail) = after_test
        .rsplit_once(result)
        .ok_or("missing test exit marker")?;
    require(
        tail.trim().is_empty(),
        "unexpected bytes after engine test result",
    )?;
    Ok(body)
}
fn summary(line: &str, passed: usize, failed: usize) -> Result<(), String> {
    summary_counts(line, passed, failed, 0)
}
fn summary_counts(line: &str, passed: usize, failed: usize, ignored: usize) -> Result<(), String> {
    summary_counts_filtered(line, passed, failed, ignored, 0)
}
fn summary_counts_filtered(
    line: &str,
    passed: usize,
    failed: usize,
    ignored: usize,
    filtered: usize,
) -> Result<(), String> {
    let prefix = if failed == 0 {
        "test result: ok. "
    } else {
        "test result: FAILED. "
    };
    let rest = line
        .strip_prefix(prefix)
        .ok_or("libtest summary status disagrees with cases")?;
    let pieces: Vec<_> = rest.split("; ").collect();
    require(pieces.len() == 6, "invalid libtest summary structure")?;
    for (piece, expected) in pieces[..5].iter().zip([
        format!("{passed} passed"),
        format!("{failed} failed"),
        format!("{ignored} ignored"),
        "0 measured".into(),
        format!("{filtered} filtered out"),
    ]) {
        require(
            *piece == expected,
            "libtest summary counts disagree with complete cases",
        )?;
    }
    let time = pieces[5]
        .strip_prefix("finished in ")
        .and_then(|v| v.strip_suffix('s'))
        .ok_or("missing libtest completion duration")?;
    require(
        time.parse::<f64>().is_ok_and(|v| v.is_finite() && v >= 0.0),
        "invalid libtest completion duration",
    )
}
/// Only case-status lines before the FIRST failures section count as observations.
/// Captured test stdout in failure details cannot inject additional case outcomes.
fn libtest(body: &str, names: &BTreeMap<String, String>, killed: bool) -> Result<Value, String> {
    let lines: Vec<_> = body.lines().collect();
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            line.starts_with("running ") && (line.ends_with(" tests") || line.ends_with(" test"))
        })
        .collect();
    require(starts.len() == 1, "expected one libtest running header")?;
    let (start, header) = starts[0];
    let suffix = if names.len() == 1 { "test" } else { "tests" };
    require(
        *header == format!("running {} {suffix}", names.len()),
        "libtest running count differs from ranked inventory",
    )?;
    let ends: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("test result: "))
        .collect();
    require(
        ends.len() == 1 && ends[0].0 > start,
        "expected one completed libtest summary",
    )?;
    let end = ends[0].0;
    let mut tests = Map::new();
    let mut failed = 0usize;
    let mut case_bytes = 256usize;
    let mut failure_details = false;
    for line in &lines[start + 1..end] {
        if line.is_empty() {
            continue;
        }
        if *line == "failures:" {
            failure_details = true;
            continue;
        }
        if failure_details {
            require(
                !line.starts_with("test "),
                "case-like line in captured failure output refused",
            )?;
            continue;
        }
        let words: Vec<_> = line.split_whitespace().collect();
        require(
            words.len() == 4 && words[0] == "test" && words[2] == "...",
            "unexpected bytes in libtest case section",
        )?;
        let node = names.get(words[1]).ok_or("unranked libtest case")?;
        let outcome = match words[3] {
            "ok" => "PASSED",
            "FAILED" => {
                failed += 1;
                "FAILED"
            }
            _ => return Err("ignored/unknown libtest case outcome".into()),
        };
        // Charge expanded node key and fixed row/envelope punctuation BEFORE
        // cloning the node into the retained normalized map.
        case_bytes = case_bytes
            .checked_add(bounded_json(node, MAX_REPORT_BYTES)? + 64)
            .ok_or("normalized case size overflow")?;
        require(
            case_bytes <= MAX_REPORT_BYTES,
            "normalized case inventory exceeds byte bound",
        )?;
        require(
            tests
                .insert(node.clone(), json!({"outcome":outcome,"cases":1}))
                .is_none(),
            "duplicate libtest case",
        )?;
    }
    require(tests.len() == names.len(), "missing libtest cases")?;
    require(
        if killed {
            failed > 0 && failure_details
        } else {
            failed == 0 && !failure_details
        },
        "scenario outcome disagrees with observed ranked failures",
    )?;
    summary(lines[end], names.len() - failed, failed)?;
    require(
        lines[end + 1..]
            .iter()
            .all(|line| line.is_empty() || line.starts_with("error: test failed, to rerun pass ")),
        "unexpected output after libtest summary",
    )?;
    let parsed = json!({"status":"COMPLETE","tests":tests,"missing_nodeids":[],"ambiguous_nodeids":[],"unmapped_cases":[],"unranked_failures":[]});
    bounded_json(&parsed, MAX_REPORT_BYTES)?;
    Ok(parsed)
}

#[path = "mutation_cargo_package_observations.rs"]
mod package_observations;
pub use package_observations::{
    parse_package_baseline, parse_package_engine_baseline, parse_selected_baseline,
    parse_selected_engine_baseline,
};

/// Strict direct-baseline stdout only. Caller checks command, ordinary exit,
/// independently executed repetitions and source binding before invoking collect.
pub fn parse_baseline(stdout: &str, target: &Target, ranked: &[String]) -> Result<Value, String> {
    require(
        stdout.len() <= MAX_LOG_BYTES && !stdout.contains('\0') && !stdout.contains('\r'),
        "baseline stdout exceeds bounds/format",
    )?;
    libtest(stdout, &names(target, ranked)?, false)
}
fn scenario_identity(value: &Value, target: &Target) -> Result<(), String> {
    let mutant = value
        .get("Mutant")
        .and_then(Value::as_object)
        .ok_or("expected engine mutant scenario")?;
    require(
        value.as_object().is_some_and(|v| v.len() == 1),
        "invalid scenario envelope",
    )?;
    let m = Value::Object(mutant.clone());
    require(safe_path(text(&m, "file")?), "invalid mutant source path")?;
    require(
        !text(&m, "name")?.is_empty() && !text(&m, "genre")?.is_empty(),
        "missing mutant identity text",
    )?;
    text(&m, "replacement")?;
    for side in ["start", "end"] {
        for axis in ["line", "column"] {
            require(
                m["span"][side][axis].as_u64().is_some_and(|n| n > 0),
                "invalid mutant span",
            )?;
        }
    }
    let start = (&m["span"]["start"]["line"], &m["span"]["start"]["column"]);
    let end = (&m["span"]["end"]["line"], &m["span"]["end"]["column"]);
    require(
        (start.0.as_u64(), start.1.as_u64()) < (end.0.as_u64(), end.1.as_u64()),
        "mutant span is empty/reversed",
    )?;
    require(
        m["package"] == target.package,
        "mutant package differs from fixed target",
    )
}
fn inventory(
    expected: &[ExpectedMutant],
    target: &Target,
) -> Result<BTreeMap<String, String>, String> {
    require(
        !expected.is_empty() && expected.len() <= MAX_MUTANTS,
        "expected mutant count exceeds bounds",
    )?;
    let mut scenarios = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for mutant in expected {
        require(
            !mutant.id.is_empty() && mutant.id.len() <= 4096 && ids.insert(mutant.id.clone()),
            "duplicate/invalid expected mutant ID",
        )?;
        scenario_identity(&mutant.scenario, target)?;
        require(
            scenarios
                .insert(mutant.scenario.to_string(), mutant.id.clone())
                .is_none(),
            "duplicate expected engine scenario",
        )?;
    }
    Ok(scenarios)
}
fn log_inventory(logs: &[ScenarioLog]) -> Result<BTreeMap<&str, &str>, String> {
    require(
        !logs.is_empty() && logs.len() <= MAX_MUTANTS + 1,
        "scenario log count exceeds bounds",
    )?;
    let mut total = 0usize;
    let mut result = BTreeMap::new();
    for log in logs {
        require(
            safe_path(&log.path) && log.path.starts_with("log/") && log.path.ends_with(".log"),
            "invalid confined scenario log path",
        )?;
        total = total
            .checked_add(log.text.len())
            .ok_or("log size overflow")?;
        require(
            log.text.len() <= MAX_LOG_BYTES
                && total <= MAX_TOTAL_LOG_BYTES
                && !log.text.contains('\0')
                && !log.text.contains('\r'),
            "scenario log bytes exceed bounds/format",
        )?;
        require(
            result
                .insert(log.path.as_str(), log.text.as_str())
                .is_none(),
            "duplicate supplied log path",
        )?;
    }
    Ok(result)
}

fn report_outcomes(report: &Value, count: usize, log_count: usize) -> Result<&[Value], String> {
    for (key, number) in [
        ("total_mutants", count),
        ("caught", count),
        ("missed", 0),
        ("timeout", 0),
        ("unviable", 0),
        ("success", 0),
    ] {
        require(
            report[key].as_u64() == Some(number as u64),
            "engine totals do not describe a complete caught-only run",
        )?;
    }
    let outcomes = array(report, "outcomes")?;
    require(
        outcomes.len() == count + 1 && log_count == outcomes.len(),
        "scenario/log inventory incomplete or extra",
    )?;
    Ok(outcomes)
}
fn charge<T: Serialize + ?Sized>(value: &T, total: &mut usize) -> Result<(), String> {
    let bytes = bounded_json(value, MAX_OUTPUT_BYTES)?;
    *total = total
        .checked_add(bytes)
        .ok_or("normalized observations size overflow")?;
    require(
        *total <= MAX_OUTPUT_BYTES,
        "normalized observations exceed incremental output bound",
    )
}
fn collect_scenarios(
    outcomes: &[Value],
    target: &Target,
    names: &BTreeMap<String, String>,
    mut expected: BTreeMap<String, String>,
    mut logs: BTreeMap<&str, &str>,
) -> Result<Observations, String> {
    let mut baseline_observation = None;
    let mut mutants = Vec::new();
    let mut reports = Map::new();
    let mut statuses = Map::new();
    // Reserve already-bounded independent baselines plus fixed envelope bytes.
    let mut charged = MAX_REPORT_BYTES + 4096;
    for value in outcomes {
        let is_baseline = value["scenario"] == "Baseline";
        let killed = !is_baseline;
        require(
            value["summary"] == if killed { "CaughtMutant" } else { "Success" },
            "scenario did not complete with expected outcome",
        )?;
        let log_path = text(value, "log_path")?;
        let log = logs.remove(log_path).ok_or("scenario log absent/reused")?;
        let (build, test) = phases(value, target, killed)?;
        let parsed = libtest(test_section(log, &build, &test, killed)?, names, killed)?;
        let evidence = json!({"log_path":log_path,"log_sha256":format!("{:x}", Sha256::digest(log.as_bytes())),"phases":value["phase_results"],"report":parsed});
        if is_baseline {
            charge(&evidence, &mut charged)?;
            require(
                baseline_observation.replace(evidence).is_none(),
                "duplicate engine baseline",
            )?;
        } else {
            scenario_identity(&value["scenario"], target)?;
            let id = expected
                .remove(&value["scenario"].to_string())
                .ok_or("engine scenario differs from expected stable-ID inventory")?;
            let observation = MutantObservation {
                id: id.clone(),
                scenario: value["scenario"].clone(),
                log_path: log_path.into(),
                log_sha256: evidence["log_sha256"].as_str().unwrap().into(),
                phases: value["phase_results"].clone(),
                report: parsed.clone(),
            };
            // Account for both retained representations and both map keys BEFORE
            // inserting another scenario; normalization may amplify short labels.
            charge(&observation, &mut charged)?;
            charge(&parsed, &mut charged)?;
            charge(&json!({"id":&id,"outcome":"KILLED"}), &mut charged)?;
            charge(&id, &mut charged)?;
            reports.insert(id.clone(), parsed);
            statuses.insert(id, json!("KILLED"));
            mutants.push(observation);
        }
    }
    require(
        expected.is_empty() && logs.is_empty(),
        "unobserved engine scenarios/logs",
    )?;
    let engine_baseline = baseline_observation.ok_or("missing engine baseline")?;
    mutants.sort_by(|a, b| a.id.cmp(&b.id));
    let output = Observations { schema_version:"llm.mutation-testing.cargo-observations.v1", scope:"caller-held original engine observations; caller establishes baseline independence and target/source provenance; no producer attestation", engine_baseline, baseline_reports:Vec::new(), mutants, mutant_reports:Value::Object(reports), mutant_outcomes:Value::Object(statuses) };
    Ok(output)
}

/// Consume only independently held bounded input bytes and coordinator-bound IDs.
/// Missing, malformed, censored, surviving or unattributed scenarios refuse value evidence.
pub fn collect(
    report_json: &str,
    logs: &[ScenarioLog],
    target: &Target,
    ranked: &[String],
    baseline_reports: &[Value],
    expected: &[ExpectedMutant],
) -> Result<Observations, String> {
    require(
        report_json.len() <= MAX_REPORT_BYTES,
        "engine report exceeds byte bound",
    )?;
    let _: DuplicateChecked =
        serde_json::from_str(report_json).map_err(|e| format!("invalid engine report: {e}"))?;
    let report = crate::mutation_evidence::parse_request(report_json)?;
    require(
        report["cargo_mutants_version"] == "27.1.0",
        "unsupported cargo-mutants report version",
    )?;
    require(
        report["end_time"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "engine report has no completion marker",
    )?;
    let names = names(target, ranked)?;
    require(
        (2..=5).contains(&baseline_reports.len()),
        "need 2..5 caller-supplied baseline observations",
    )?;
    require(
        bounded_json(baseline_reports, MAX_REPORT_BYTES).is_ok(),
        "baseline inputs exceed bound",
    )?;
    for value in baseline_reports {
        baseline(value, &names)?;
    }
    bounded_json(expected, MAX_REPORT_BYTES)?;
    let expected = inventory(expected, target)?;
    let logs = log_inventory(logs)?;
    let count = expected.len();
    let outcomes = report_outcomes(&report, count, logs.len())?;
    let mut output = collect_scenarios(outcomes, target, &names, expected, logs)?;
    output.baseline_reports = baseline_reports.to_vec();
    require(
        bounded_json(&output, MAX_OUTPUT_BYTES).is_ok(),
        "normalized observations exceed output bound",
    )?;
    Ok(output)
}

/// Duplicate-only preflight: do not reconstruct numbers from arbitrary_precision's
/// private numeric map representation. Shared parse_request retains actual Values.
struct DuplicateChecked;
impl<'de> Deserialize<'de> for DuplicateChecked {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = DuplicateChecked;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("duplicate-free JSON")
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(DuplicateChecked)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                while seq.next_element::<DuplicateChecked>()?.is_some() {}
                Ok(DuplicateChecked)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut keys = BTreeSet::new();
                while let Some((key, _)) = map.next_entry::<String, DuplicateChecked>()? {
                    if !keys.insert(key) {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(DuplicateChecked)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
#[cfg(test)]
#[path = "mutation_cargo_observations_tests.rs"]
mod tests;
