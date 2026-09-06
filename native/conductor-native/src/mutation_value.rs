//! Deterministic mutation-test value analysis and fail-closed receipt validation.

use std::collections::{HashMap, HashSet};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};

const VALUE_SCHEMA: &str = "llm.mutation-testing.test-value.v1";
const ADAPTERS: [&str; 3] = ["pytest-junit", "ctest-junit", "cargo-libtest"];
const CLASSIFICATIONS: [&str; 4] = [
    "CORE",
    "INTENTIONAL_REDUNDANCY",
    "MERGE",
    "DELETE_CANDIDATE",
];

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ValueContract {
    id: String,
    criticality: String,
    active_paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ValueTest {
    nodeid: String,
    contract_id: String,
    intentional_redundancy: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ValueSpec {
    adapter: String,
    baseline_repetitions: usize,
    contracts: Vec<ValueContract>,
    tests: Vec<ValueTest>,
    mutation_contracts: Vec<(String, String)>,
}

#[derive(Debug, Deserialize)]
struct MutantEvidence {
    mutation_id: String,
    outcome: Value,
    report_state: String,
    killers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawAnalysis {
    enabled: Option<Value>,
    adapter: Option<Value>,
    baseline_repetitions: Option<Value>,
    required_contracts: Option<Value>,
    tests: Option<Value>,
    mutation_contracts: Option<Value>,
}

#[derive(Debug, Serialize)]
struct TestRow {
    nodeid: String,
    contract_id: String,
    classification: String,
    killed_mutants: Vec<String>,
    unique_kills: Vec<String>,
    runtime_seconds_median: Option<f64>,
    baseline_outcomes: Vec<String>,
    dominated_by: Vec<String>,
}

struct OrderedStringVecMap(Vec<(String, Vec<String>)>);

impl Serialize for OrderedStringVecMap {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap; // codespell:ignore ser
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

struct OrderedCounts(Vec<(&'static str, usize)>);

impl Serialize for OrderedCounts {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap; // codespell:ignore ser
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

#[derive(Serialize)]
struct AnalysisOutput {
    schema_version: &'static str,
    status: &'static str,
    adapter: String,
    baseline_repetitions: usize,
    subprocess_scaling: &'static str,
    errors: Vec<String>,
    required_contracts: Vec<ValueContract>,
    killers_by_mutant: OrderedStringVecMap,
    retained_core: Vec<String>,
    tests: Vec<TestRow>,
    classification_counts: OrderedCounts,
}

fn value_error(message: impl Into<String>) -> PyErr {
    PyValueError::new_err(message.into())
}

fn python_repr(value: Option<&Value>) -> String {
    match value.unwrap_or(&Value::Null) {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'")),
        other => other.to_string(),
    }
}

fn python_string_list(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| python_repr(Some(&Value::String(value.clone()))))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn required_object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{label} must be an object"))
}

fn required_string(value: Option<&Value>, label: &str) -> Result<String, String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{label} must be a non-empty string"))
}

fn safe_path(value: Option<&Value>, label: &str) -> Result<String, String> {
    let text = required_string(value, label)?.replace('\\', "/");
    if text.starts_with('/') || text.starts_with("./") || text.split('/').any(|part| part == "..") {
        return Err(format!("{label} must be repository-relative"));
    }
    let normalized = text
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/");
    Ok(if normalized.is_empty() {
        ".".to_owned()
    } else {
        normalized
    })
}

fn parse_pairs(text: &str) -> Result<Vec<(Value, Value)>, String> {
    serde_json::from_str(text).map_err(|error| format!("invalid mutation contract order: {error}"))
}

/// Whether a ranked nodeid's file hosts its own tests instead of being a test file.
///
/// Rust unit tests live in a `#[cfg(test)] mod` inside the module they exercise, so the
/// path of a cargo nodeid is production source. Refusing it as a contract's active path
/// would make value analysis unreachable for every in-module Rust campaign. Python test
/// files are separate files and stay excluded.
fn is_in_module_test_path(path: &str) -> bool {
    path.ends_with(".rs")
}

fn load_spec(
    value_json: &str,
    mutation_pairs_json: &str,
    ranked_nodeids: &[String],
    mutation_ids: &[String],
    source_paths: &[String],
) -> Result<Option<ValueSpec>, String> {
    let value: Value = serde_json::from_str(value_json)
        .map_err(|error| format!("invalid value_analysis JSON: {error}"))?;
    if value.is_null() {
        return Ok(None);
    }
    required_object(&value, "value_analysis")?;
    let raw: RawAnalysis = serde_json::from_value(value)
        .map_err(|error| format!("value_analysis must be an object: {error}"))?;
    if raw.enabled != Some(Value::Bool(true)) {
        return Err("value_analysis.enabled must be true when present".to_owned());
    }
    let adapter = required_string(raw.adapter.as_ref(), "value_analysis.adapter")?;
    if !ADAPTERS.contains(&adapter.as_str()) {
        return Err(format!(
            "value_analysis.adapter must be one of {}",
            python_string_list(&ADAPTERS.map(str::to_owned))
        ));
    }
    let repetitions = raw
        .baseline_repetitions
        .as_ref()
        .and_then(Value::as_i64)
        .filter(|value| (2..=5).contains(value))
        .ok_or_else(|| "baseline_repetitions must be an integer in [2, 5]".to_owned())?
        as usize;

    let raw_contracts = raw
        .required_contracts
        .as_ref()
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
        .ok_or_else(|| "required_contracts must be a non-empty list".to_owned())?;
    let test_paths: HashSet<&str> = ranked_nodeids
        .iter()
        .map(|nodeid| nodeid.split("::").next().unwrap_or(nodeid))
        .filter(|path| !is_in_module_test_path(path))
        .collect();
    let source_path_set: HashSet<&str> = source_paths.iter().map(String::as_str).collect();
    let mut contracts = Vec::with_capacity(raw_contracts.len());
    for (index, value) in raw_contracts.iter().enumerate() {
        let row = required_object(value, &format!("required_contracts[{index}]"))?;
        let id = required_string(row.get("id"), &format!("required_contracts[{index}].id"))?;
        let criticality = required_string(
            row.get("criticality"),
            &format!("required_contracts[{index}].criticality"),
        )?;
        if criticality != "critical" && criticality != "high" {
            return Err(format!(
                "required_contracts[{index}].criticality must be critical or high"
            ));
        }
        let raw_paths = row
            .get("active_paths")
            .and_then(Value::as_array)
            .filter(|paths| !paths.is_empty())
            .ok_or_else(|| format!("required_contracts[{index}].active_paths must be non-empty"))?;
        let label = format!("required_contracts[{index}].active_paths");
        let active_paths = raw_paths
            .iter()
            .map(|path| safe_path(Some(path), &label))
            .collect::<Result<Vec<_>, _>>()?;
        let mut unbound = active_paths
            .iter()
            .filter(|path| !source_path_set.contains(path.as_str()))
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        unbound.sort();
        if !unbound.is_empty() {
            return Err(format!(
                "contract {} has unbound active paths: {}",
                python_repr(Some(&Value::String(id.clone()))),
                python_string_list(&unbound)
            ));
        }
        let mut test_targets = active_paths
            .iter()
            .filter(|path| test_paths.contains(path.as_str()))
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        test_targets.sort();
        if !test_targets.is_empty() {
            return Err(format!(
                "contract {} active paths are tests, not production: {}",
                python_repr(Some(&Value::String(id.clone()))),
                python_string_list(&test_targets)
            ));
        }
        contracts.push(ValueContract {
            id,
            criticality,
            active_paths,
        });
    }
    let mut contract_ids = HashSet::new();
    if contracts
        .iter()
        .any(|contract| !contract_ids.insert(contract.id.clone()))
    {
        return Err("required_contracts contains duplicate ids".to_owned());
    }

    let raw_tests = raw
        .tests
        .as_ref()
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
        .ok_or_else(|| "value_analysis.tests must be a non-empty list".to_owned())?;
    let mut tests = Vec::with_capacity(raw_tests.len());
    for (index, value) in raw_tests.iter().enumerate() {
        let row = required_object(value, &format!("value_analysis.tests[{index}]"))?;
        let intentional_redundancy = match row.get("intentional_redundancy") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                return Err(format!(
                    "value_analysis.tests[{index}].intentional_redundancy must be boolean"
                ))
            }
        };
        tests.push(ValueTest {
            nodeid: required_string(
                row.get("nodeid"),
                &format!("value_analysis.tests[{index}].nodeid"),
            )?,
            contract_id: required_string(
                row.get("contract_id"),
                &format!("value_analysis.tests[{index}].contract_id"),
            )?,
            intentional_redundancy,
        });
    }
    if tests
        .iter()
        .map(|test| &test.nodeid)
        .ne(ranked_nodeids.iter())
    {
        return Err(
            "value_analysis.tests must exactly match ranked_tests in rank order".to_owned(),
        );
    }
    let mut unknown = tests
        .iter()
        .filter(|test| !contract_ids.contains(&test.contract_id))
        .map(|test| test.contract_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    unknown.sort();
    if !unknown.is_empty() {
        return Err(format!(
            "tests reference unknown contracts: {}",
            python_string_list(&unknown)
        ));
    }

    if !raw
        .mutation_contracts
        .as_ref()
        .is_some_and(Value::is_object)
    {
        return Err("value_analysis.mutation_contracts must be an object".to_owned());
    }
    let pairs = parse_pairs(mutation_pairs_json)?;
    let mut mutation_contracts = Vec::with_capacity(pairs.len());
    for (mutation_id, contract_id) in pairs {
        let mutation_id = required_string(Some(&mutation_id), "mutation contract id")?;
        let contract_id = required_string(
            Some(&contract_id),
            &format!(
                "mutation_contracts[{}]",
                python_repr(Some(&Value::String(mutation_id.clone())))
            ),
        )?;
        mutation_contracts.push((mutation_id, contract_id));
    }
    if mutation_contracts
        .iter()
        .map(|(id, _)| id)
        .ne(mutation_ids.iter())
    {
        return Err("mutation_contracts must exactly match planned mutations in order".to_owned());
    }
    let mut unknown = mutation_contracts
        .iter()
        .filter(|(_, contract)| !contract_ids.contains(contract))
        .map(|(_, contract)| contract.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    unknown.sort();
    if !unknown.is_empty() {
        return Err(format!(
            "mutations reference unknown contracts: {}",
            python_string_list(&unknown)
        ));
    }
    let test_contracts: HashSet<&str> =
        tests.iter().map(|test| test.contract_id.as_str()).collect();
    let mutant_contracts: HashSet<&str> = mutation_contracts
        .iter()
        .map(|(_, contract)| contract.as_str())
        .collect();
    let mut missing_tests = contract_ids
        .iter()
        .filter(|id| !test_contracts.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let mut missing_mutants = contract_ids
        .iter()
        .filter(|id| !mutant_contracts.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    missing_tests.sort();
    missing_mutants.sort();
    if !missing_tests.is_empty() || !missing_mutants.is_empty() {
        return Err(format!(
            "every contract needs tests and mutants; missing_tests={}, missing_mutants={}",
            python_string_list(&missing_tests),
            python_string_list(&missing_mutants)
        ));
    }
    Ok(Some(ValueSpec {
        adapter,
        baseline_repetitions: repetitions,
        contracts,
        tests,
        mutation_contracts,
    }))
}

fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    Some(if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    })
}

fn round_six(value: f64) -> f64 {
    format!("{value:.6}").parse().unwrap_or(value)
}

fn report_tests(report: &Value) -> Option<&Map<String, Value>> {
    report.get("tests").and_then(Value::as_object)
}

fn analyze(
    spec: ValueSpec,
    baseline_reports: Vec<Value>,
    mutant_evidence: Vec<MutantEvidence>,
) -> AnalysisOutput {
    let nodeids = spec
        .tests
        .iter()
        .map(|test| test.nodeid.clone())
        .collect::<Vec<_>>();
    let tests_by_nodeid = spec
        .tests
        .iter()
        .map(|test| (test.nodeid.as_str(), test))
        .collect::<HashMap<_, _>>();
    let mut baseline_outcomes = nodeids
        .iter()
        .map(|nodeid| (nodeid.clone(), Vec::new()))
        .collect::<HashMap<_, Vec<String>>>();
    let mut baseline_durations = nodeids
        .iter()
        .map(|nodeid| (nodeid.clone(), Vec::new()))
        .collect::<HashMap<_, Vec<f64>>>();
    let mut errors = Vec::new();
    if baseline_reports.len() != spec.baseline_repetitions {
        errors.push(format!(
            "baseline repetition count mismatch: expected={}, actual={}",
            spec.baseline_repetitions,
            baseline_reports.len()
        ));
    }
    for (index, report) in baseline_reports.iter().enumerate() {
        if report.get("status").and_then(Value::as_str) != Some("COMPLETE") {
            errors.push(format!("baseline report {} is incomplete", index + 1));
        }
        let Some(tests) = report_tests(report) else {
            errors.push(format!("baseline report {} has no test map", index + 1));
            continue;
        };
        for nodeid in &nodeids {
            let Some(row) = tests.get(nodeid).and_then(Value::as_object) else {
                continue;
            };
            if let Some(outcome) = row.get("outcome").and_then(Value::as_str) {
                baseline_outcomes
                    .get_mut(nodeid)
                    .expect("known nodeid")
                    .push(outcome.to_owned());
            }
            if let Some(duration) = row.get("duration_seconds").and_then(Value::as_f64) {
                baseline_durations
                    .get_mut(nodeid)
                    .expect("known nodeid")
                    .push(duration);
            }
        }
    }
    let flaky = nodeids
        .iter()
        .filter(|nodeid| {
            let outcomes = &baseline_outcomes[*nodeid];
            outcomes.len() != spec.baseline_repetitions
                || outcomes.iter().any(|outcome| outcome != "PASSED")
        })
        .cloned()
        .collect::<Vec<_>>();
    if !flaky.is_empty() {
        errors.push(format!(
            "baseline instability or non-pass outcomes: {}",
            python_string_list(&flaky)
        ));
    }

    let mut kills_by_test = nodeids
        .iter()
        .map(|nodeid| (nodeid.clone(), HashSet::new()))
        .collect::<HashMap<_, HashSet<String>>>();
    let mut killers_by_mutant = Vec::with_capacity(spec.mutation_contracts.len());
    let evidence_by_id = mutant_evidence
        .into_iter()
        .map(|evidence| (evidence.mutation_id.clone(), evidence))
        .collect::<HashMap<_, _>>();
    for (mutation_id, contract_id) in &spec.mutation_contracts {
        let evidence = evidence_by_id.get(mutation_id);
        let outcome = evidence.map(|row| &row.outcome).unwrap_or(&Value::Null);
        let outcome_text = outcome.as_str();
        if outcome_text != Some("KILLED") {
            errors.push(format!(
                "mutant {} outcome is {}",
                python_repr(Some(&Value::String(mutation_id.clone()))),
                python_repr(Some(outcome))
            ));
        }
        let Some(evidence) = evidence else {
            errors.push(format!(
                "mutant {} attribution is incomplete",
                python_repr(Some(&Value::String(mutation_id.clone())))
            ));
            killers_by_mutant.push((mutation_id.clone(), Vec::new()));
            continue;
        };
        if evidence.report_state == "INCOMPLETE" {
            errors.push(format!(
                "mutant {} attribution is incomplete",
                python_repr(Some(&Value::String(mutation_id.clone())))
            ));
            killers_by_mutant.push((mutation_id.clone(), Vec::new()));
            continue;
        }
        if evidence.report_state == "NO_TEST_MAP" {
            errors.push(format!(
                "mutant {} has no test map",
                python_repr(Some(&Value::String(mutation_id.clone())))
            ));
            killers_by_mutant.push((mutation_id.clone(), Vec::new()));
            continue;
        }
        let killers = evidence.killers.clone();
        for nodeid in &killers {
            kills_by_test
                .get_mut(nodeid)
                .expect("known nodeid")
                .insert(mutation_id.clone());
        }
        if !killers
            .iter()
            .any(|nodeid| tests_by_nodeid[nodeid.as_str()].contract_id == *contract_id)
        {
            errors.push(format!(
                "mutant {} has no killer bound to contract {}",
                python_repr(Some(&Value::String(mutation_id.clone()))),
                python_repr(Some(&Value::String(contract_id.clone())))
            ));
        }
        killers_by_mutant.push((mutation_id.clone(), killers));
    }

    let contract_index = spec
        .contracts
        .iter()
        .enumerate()
        .map(|(index, contract)| (contract.id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mutant_index = spec
        .mutation_contracts
        .iter()
        .enumerate()
        .map(|(index, (id, _))| (id.as_str(), spec.contracts.len() + index))
        .collect::<HashMap<_, _>>();
    let universe_len = spec.contracts.len() + spec.mutation_contracts.len();
    let word_count = universe_len.div_ceil(64);
    let mut universe = vec![u64::MAX; word_count];
    if let Some(last) = universe.last_mut() {
        let final_bits = universe_len % 64;
        if final_bits != 0 {
            *last = (1_u64 << final_bits) - 1;
        }
    }
    let coverage = spec
        .tests
        .iter()
        .map(|test| {
            let mut bits = vec![0_u64; word_count];
            let contract = contract_index[test.contract_id.as_str()];
            bits[contract / 64] |= 1_u64 << (contract % 64);
            for mutation_id in &kills_by_test[&test.nodeid] {
                let mutation = mutant_index[mutation_id.as_str()];
                bits[mutation / 64] |= 1_u64 << (mutation % 64);
            }
            bits
        })
        .collect::<Vec<_>>();
    let runtimes = nodeids
        .iter()
        .map(|nodeid| median(&baseline_durations[nodeid]))
        .collect::<Vec<_>>();
    let mut uncovered = universe.clone();
    let mut core_indices = Vec::new();
    while uncovered.iter().any(|word| *word != 0) {
        let best = (0..nodeids.len())
            .filter(|index| {
                coverage[*index]
                    .iter()
                    .zip(&uncovered)
                    .any(|(covered, missing)| covered & missing != 0)
            })
            .min_by(|left, right| {
                let new_count = |index: usize| {
                    coverage[index]
                        .iter()
                        .zip(&uncovered)
                        .map(|(covered, missing)| (covered & missing).count_ones())
                        .sum::<u32>()
                };
                let left_new = new_count(*left);
                let right_new = new_count(*right);
                right_new
                    .cmp(&left_new)
                    .then_with(|| {
                        runtimes[*left]
                            .unwrap_or(f64::INFINITY)
                            .total_cmp(&runtimes[*right].unwrap_or(f64::INFINITY))
                    })
                    .then_with(|| left.cmp(right))
                    .then_with(|| nodeids[*left].cmp(&nodeids[*right]))
            });
        let Some(best) = best else {
            core_indices.clear();
            break;
        };
        for (missing, covered) in uncovered.iter_mut().zip(&coverage[best]) {
            *missing &= !covered;
        }
        core_indices.push(best);
    }
    for index in core_indices.clone().into_iter().rev() {
        let reduced = core_indices
            .iter()
            .filter(|item| **item != index)
            .cloned()
            .collect::<Vec<_>>();
        let mut covered = vec![0_u64; word_count];
        for selected in &reduced {
            for (combined, word) in covered.iter_mut().zip(&coverage[*selected]) {
                *combined |= word;
            }
        }
        if universe
            .iter()
            .zip(&covered)
            .all(|(all, found)| all & !found == 0)
        {
            core_indices = reduced;
        }
    }
    if core_indices.is_empty() {
        errors.push("no retained test set covers every contract and mutant".to_owned());
    }

    let core_set = core_indices.iter().copied().collect::<HashSet<_>>();
    let killer_map = killers_by_mutant.iter().cloned().collect::<HashMap<_, _>>();
    let rows = spec
        .tests
        .iter()
        .enumerate()
        .map(|(test_index, test)| {
            let mut kills = kills_by_test[&test.nodeid]
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            kills.sort();
            let classification = if core_set.contains(&test_index) {
                "CORE"
            } else if test.intentional_redundancy && !kills.is_empty() {
                "INTENTIONAL_REDUNDANCY"
            } else if !kills.is_empty() {
                "MERGE"
            } else {
                "DELETE_CANDIDATE"
            };
            let unique_kills = kills
                .iter()
                .filter(|mutation_id| {
                    killer_map.get(*mutation_id) == Some(&vec![test.nodeid.clone()])
                })
                .cloned()
                .collect();
            let mut dominated_by = nodeids
                .iter()
                .enumerate()
                .filter(|(other_index, _)| {
                    *other_index != test_index
                        && coverage[test_index]
                            .iter()
                            .zip(&coverage[*other_index])
                            .all(|(this, other)| this & !other == 0)
                        && match (runtimes[*other_index], runtimes[test_index]) {
                            (Some(left), Some(right)) => left <= right,
                            (Some(_), None) => true,
                            (None, None) => true,
                            (None, Some(_)) => false,
                        }
                })
                .map(|(_, nodeid)| nodeid.clone())
                .collect::<Vec<_>>();
            dominated_by.sort();
            TestRow {
                nodeid: test.nodeid.clone(),
                contract_id: test.contract_id.clone(),
                classification: classification.to_owned(),
                killed_mutants: kills,
                unique_kills,
                runtime_seconds_median: runtimes[test_index].map(round_six),
                baseline_outcomes: baseline_outcomes[&test.nodeid].clone(),
                dominated_by,
            }
        })
        .collect::<Vec<_>>();
    let counts = CLASSIFICATIONS
        .iter()
        .map(|classification| {
            (
                *classification,
                rows.iter()
                    .filter(|row| row.classification == *classification)
                    .count(),
            )
        })
        .collect();
    AnalysisOutput {
        schema_version: VALUE_SCHEMA,
        status: if errors.is_empty() {
            "PASS"
        } else {
            "FAIL_CLOSED"
        },
        adapter: spec.adapter,
        baseline_repetitions: spec.baseline_repetitions,
        subprocess_scaling: "baseline_repetitions + mutants",
        errors,
        required_contracts: spec.contracts,
        killers_by_mutant: OrderedStringVecMap(killers_by_mutant),
        retained_core: core_indices
            .into_iter()
            .map(|index| nodeids[index].clone())
            .collect(),
        tests: rows,
        classification_counts: OrderedCounts(counts),
    }
}

fn parse_json(text: &str, label: &str) -> PyResult<Value> {
    serde_json::from_str(text).map_err(|error| value_error(format!("invalid {label}: {error}")))
}

#[pyfunction]
fn load_value_analysis_native(
    value_json: &str,
    mutation_pairs_json: &str,
    ranked_nodeids: Vec<String>,
    mutation_ids: Vec<String>,
    source_paths: Vec<String>,
) -> PyResult<Option<String>> {
    load_spec(
        value_json,
        mutation_pairs_json,
        &ranked_nodeids,
        &mutation_ids,
        &source_paths,
    )
    .map(|spec| spec.map(|value| serde_json::to_string(&value).expect("spec is serializable")))
    .map_err(value_error)
}

#[pyfunction]
fn analyze_test_value_native(
    spec_json: &str,
    baseline_reports_json: &str,
    mutant_evidence_json: &str,
) -> PyResult<String> {
    let spec = serde_json::from_str(spec_json)
        .map_err(|error| value_error(format!("invalid value spec: {error}")))?;
    let baseline_reports = serde_json::from_str(baseline_reports_json)
        .map_err(|error| value_error(format!("invalid baseline reports: {error}")))?;
    let mutant_evidence = serde_json::from_str(mutant_evidence_json)
        .map_err(|error| value_error(format!("invalid mutant evidence: {error}")))?;
    serde_json::to_string(&analyze(spec, baseline_reports, mutant_evidence))
        .map_err(|error| value_error(error.to_string()))
}

#[pyfunction]
fn admission_errors_native(
    value_json: &str,
    required_nodeids: Vec<String>,
) -> PyResult<Vec<String>> {
    let value = parse_json(value_json, "test_value evidence")?;
    let Some(payload) = value.as_object() else {
        return Ok(vec!["receipt has no test_value evidence".to_owned()]);
    };
    let mut errors = Vec::new();
    if payload.get("schema_version").and_then(Value::as_str) != Some(VALUE_SCHEMA) {
        errors.push("test_value schema is not current".to_owned());
    }
    if payload.get("status").and_then(Value::as_str) != Some("PASS") {
        errors.push(format!(
            "test_value status={}",
            python_repr(payload.get("status"))
        ));
    }
    let Some(rows) = payload.get("tests").and_then(Value::as_array) else {
        errors.push("test_value.tests must be a list".to_owned());
        return Ok(errors);
    };
    let by_nodeid = rows
        .iter()
        .filter_map(|row| row.as_object())
        .filter_map(|row| {
            row.get("nodeid")
                .and_then(Value::as_str)
                .map(|nodeid| (nodeid, row))
        })
        .collect::<HashMap<_, _>>();
    for nodeid in required_nodeids {
        let Some(row) = by_nodeid.get(nodeid.as_str()) else {
            errors.push(format!(
                "new test {} has no value classification",
                python_repr(Some(&Value::String(nodeid)))
            ));
            continue;
        };
        let classification = row.get("classification");
        if !classification
            .and_then(Value::as_str)
            .is_some_and(|value| value == "CORE" || value == "INTENTIONAL_REDUNDANCY")
        {
            errors.push(format!(
                "new test {} is classified {}; only CORE or INTENTIONAL_REDUNDANCY may be added",
                python_repr(Some(&Value::String(nodeid))),
                python_repr(classification)
            ));
        }
    }
    Ok(errors)
}

#[pyfunction]
fn test_value_receipt_errors_native(
    value_json: &str,
    expected_nodeids: Vec<String>,
    expected_repetitions: i64,
) -> PyResult<Vec<String>> {
    let value = parse_json(value_json, "test_value evidence")?;
    let Some(payload) = value.as_object() else {
        return Ok(vec!["test_value evidence is missing".to_owned()]);
    };
    let mut errors = Vec::new();
    if payload.get("schema_version").and_then(Value::as_str) != Some(VALUE_SCHEMA) {
        errors.push("test_value schema is not current".to_owned());
    }
    if payload.get("status").and_then(Value::as_str) != Some("PASS") {
        errors.push(format!(
            "test_value status={}",
            python_repr(payload.get("status"))
        ));
    }
    let actual_nodeids = payload
        .get("tests")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_object)
                .map(|row| row.get("nodeid").cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let expected = expected_nodeids
        .into_iter()
        .map(Value::String)
        .collect::<Vec<_>>();
    if actual_nodeids != expected {
        errors.push("test_value nodeids do not match ranked tests".to_owned());
    }
    if payload.get("baseline_repetitions").and_then(Value::as_i64) != Some(expected_repetitions) {
        errors.push("test_value baseline repetitions mismatch".to_owned());
    }
    Ok(errors)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(load_value_analysis_native, module)?)?;
    module.add_function(wrap_pyfunction!(analyze_test_value_native, module)?)?;
    module.add_function(wrap_pyfunction!(admission_errors_native, module)?)?;
    module.add_function(wrap_pyfunction!(test_value_receipt_errors_native, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_for(path: &str) -> Result<Option<ValueSpec>, String> {
        let value = format!(
            r#"{{"enabled": true, "adapter": "pytest-junit", "baseline_repetitions": 2,
                 "required_contracts": [
                   {{"id": "c", "criticality": "critical", "active_paths": ["{path}"]}}],
                 "tests": [{{"nodeid": "{path}::t", "contract_id": "c"}}],
                 "mutation_contracts": {{"m": "c"}}}}"#
        );
        load_spec(
            &value,
            r#"[["m", "c"]]"#,
            &[format!("{path}::t")],
            &["m".to_owned()],
            &[path.to_owned()],
        )
    }

    #[test]
    fn a_rust_source_may_carry_its_own_contracts() {
        let spec = spec_for("tooling/native/conductor-native/src/mutation_value.rs")
            .expect("in-module rust tests must not refuse their own source");
        assert_eq!(spec.expect("spec").contracts.len(), 1);
    }

    #[test]
    fn a_python_test_file_is_still_refused_as_a_contract_path() {
        let error = spec_for("conductor/test_mutation_value.py")
            .expect_err("a python test file is not production");
        assert!(
            error.contains("active paths are tests, not production"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn only_rust_paths_are_exempt_from_the_test_path_refusal() {
        assert!(is_in_module_test_path("a/b/c.rs"));
        assert!(!is_in_module_test_path("a/b/test_c.py"));
        assert!(!is_in_module_test_path("a/b/c.rs.py"));
    }
}
