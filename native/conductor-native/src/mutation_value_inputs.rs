//! Bounded, interpreter-free parsing of mutation value result inputs.
//!
//! The Python extension and `forge` consume the same outcome maps. A report
//! that cannot be parsed is an error; callers turn that into incomplete
//! attribution rather than crediting a mutant with an unobserved kill.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::Path;

const MAX_REPORT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CASES: usize = 200_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JunitAdapter {
    Pytest,
    Ctest,
}

impl JunitAdapter {
    pub fn from_name(name: &str) -> Result<Self, String> {
        match name {
            "pytest-junit" => Ok(Self::Pytest),
            "ctest-junit" => Ok(Self::Ctest),
            _ => Err(format!("unknown JUnit adapter: {name}")),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pytest => "pytest",
            Self::Ctest => "ctest",
        }
    }
}

pub fn pytest_identity(nodeid: &str) -> Result<(String, String), String> {
    let parts = nodeid.split("::").collect::<Vec<_>>();
    let module = parts[0];
    if !module.ends_with(".py") || parts.len() < 2 {
        return Err(format!("pytest-junit requires a Python nodeid: {nodeid}"));
    }
    let mut classname = module[..module.len() - 3].replace('/', ".");
    if parts.len() > 2 {
        classname.push('.');
        classname.push_str(&parts[1..parts.len() - 1].join("."));
    }
    Ok((classname, parts[parts.len() - 1].to_owned()))
}

pub fn cargo_identity(nodeid: &str) -> Result<String, String> {
    let (path, function) = nodeid
        .split_once("::")
        .ok_or_else(|| format!("cargo-libtest requires a Rust nodeid: {nodeid}"))?;
    if !path.ends_with(".rs") || function.is_empty() || function.contains("::") {
        return Err(format!("cargo-libtest requires a Rust nodeid: {nodeid}"));
    }
    Ok(function.to_owned())
}

pub fn ctest_identity(nodeid: &str) -> Result<String, String> {
    let (path, function) = nodeid
        .split_once("::")
        .ok_or_else(|| format!("ctest requires a C/C++ nodeid: {nodeid}"))?;
    if function.is_empty()
        || function.contains("::")
        || ![".c", ".cc", ".cpp", ".cxx"]
            .iter()
            .any(|suffix| path.ends_with(suffix))
    {
        return Err(format!("ctest requires a C/C++ nodeid: {nodeid}"));
    }
    let stem = Path::new(path)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    Ok(format!("{stem}.{function}"))
}

pub fn attribution_supported(adapter: &str, ranked: &[String]) -> bool {
    if ranked.is_empty() {
        return false;
    }
    let names = ranked.iter().map(|nodeid| match adapter {
        "pytest-junit" => pytest_identity(nodeid).map(|(class, name)| format!("{class}\0{name}")),
        "cargo-libtest" => cargo_identity(nodeid),
        "ctest-junit" => ctest_identity(nodeid),
        _ => Err(format!("unknown attribution adapter: {adapter}")),
    });
    let mut seen = HashSet::new();
    for name in names {
        let Ok(name) = name else { return false };
        if adapter != "pytest-junit" && !seen.insert(name) {
            return false;
        }
    }
    true
}

#[derive(Default)]
struct XmlCase {
    name: String,
    classname: String,
    status: String,
    time: String,
    error: bool,
    failure: bool,
    skipped: bool,
    depth: usize,
}

impl XmlCase {
    fn outcome(&self, adapter: JunitAdapter) -> &'static str {
        if self.error {
            "ERROR"
        } else if self.failure || (adapter == JunitAdapter::Ctest && self.status == "fail") {
            "FAILED"
        } else if self.skipped
            || (adapter == JunitAdapter::Ctest
                && matches!(self.status.as_str(), "disabled" | "notrun"))
        {
            "SKIPPED"
        } else {
            "PASSED"
        }
    }

    fn duration(&self) -> f64 {
        let parsed = self.time.trim().parse::<f64>().unwrap_or(0.0);
        if parsed.is_finite() {
            parsed
        } else {
            0.0
        }
    }
}

fn xml_case(
    start: &BytesStart<'_>,
    reader: &Reader<impl BufRead>,
    depth: usize,
) -> Result<XmlCase, String> {
    let mut case = XmlCase {
        depth,
        ..XmlCase::default()
    };
    for attr in start.attributes() {
        let attr = attr.map_err(|error| error.to_string())?;
        let value = attr
            .decode_and_unescape_value(reader.decoder())
            .map_err(|error| error.to_string())?
            .into_owned();
        match attr.key.as_ref() {
            b"name" => case.name = value,
            b"classname" => case.classname = value,
            b"status" => case.status = value,
            b"time" => case.time = value,
            _ => {}
        }
    }
    Ok(case)
}

fn round_six(value: f64) -> f64 {
    (value * 1_000_000.0).round_ties_even() / 1_000_000.0
}

struct JunitRows {
    adapter: JunitAdapter,
    ranked: Vec<String>,
    pytest_ids: HashMap<(String, String), String>,
    ctest_ids: HashMap<String, String>,
    tests: Map<String, Value>,
    unmapped: Vec<Value>,
    unranked_failures: Vec<String>,
    cases: usize,
}

impl JunitRows {
    fn new(adapter: JunitAdapter, ranked: &[String]) -> Result<Self, String> {
        let mut pytest_ids = HashMap::new();
        let mut ctest_ids = HashMap::new();
        for nodeid in ranked {
            match adapter {
                JunitAdapter::Pytest => {
                    pytest_ids.insert(pytest_identity(nodeid)?, nodeid.clone());
                }
                JunitAdapter::Ctest => {
                    ctest_ids.insert(ctest_identity(nodeid)?, nodeid.clone());
                }
            }
        }
        Ok(Self {
            adapter,
            ranked: ranked.to_vec(),
            pytest_ids,
            ctest_ids,
            tests: Map::new(),
            unmapped: Vec::new(),
            unranked_failures: Vec::new(),
            cases: 0,
        })
    }

    fn consume(&mut self, case: XmlCase) -> Result<(), String> {
        self.cases += 1;
        if self.cases > MAX_CASES {
            return Err(format!(
                "JUnit report exceeds {MAX_CASES} testcase elements"
            ));
        }
        match self.adapter {
            JunitAdapter::Pytest => self.consume_pytest(case),
            JunitAdapter::Ctest => self.consume_ctest(case),
        }
        Ok(())
    }

    fn consume_pytest(&mut self, case: XmlCase) {
        let bare_name = case.name.split('[').next().unwrap_or_default();
        let Some(nodeid) = self
            .pytest_ids
            .get(&(case.classname.clone(), bare_name.to_owned()))
        else {
            self.unmapped
                .push(json!({"classname": case.classname, "name": case.name}));
            return;
        };
        let outcome = case.outcome(self.adapter);
        let row = self
            .tests
            .entry(nodeid.clone())
            .or_insert_with(|| json!({"outcome": "PASSED", "duration_seconds": 0.0, "cases": 0}));
        let cases = row["cases"].as_u64().unwrap_or(0);
        let prior = row["outcome"].as_str().unwrap_or("PASSED");
        let next = if outcome == "ERROR"
            || (outcome == "FAILED" && prior != "ERROR")
            || (outcome == "SKIPPED" && cases == 0)
            || (outcome == "PASSED" && prior == "SKIPPED")
        {
            outcome
        } else {
            prior
        };
        let total = row["duration_seconds"].as_f64().unwrap_or(0.0) + case.duration();
        row["outcome"] = json!(next);
        row["duration_seconds"] = json!(total);
        row["cases"] = json!(cases + 1);
    }

    fn consume_ctest(&mut self, case: XmlCase) {
        let outcome = case.outcome(self.adapter);
        let Some(nodeid) = self.ctest_ids.get(&case.name) else {
            if matches!(outcome, "FAILED" | "ERROR") {
                self.unranked_failures.push(case.name);
            }
            return;
        };
        self.tests.insert(
            nodeid.clone(),
            json!({
                "outcome": outcome, "duration_seconds": round_six(case.duration()), "cases": 1
            }),
        );
    }

    fn finish(mut self) -> Value {
        if self.adapter == JunitAdapter::Pytest {
            for row in self.tests.values_mut() {
                row["duration_seconds"] =
                    json!(round_six(row["duration_seconds"].as_f64().unwrap_or(0.0)));
            }
        }
        let missing = self
            .ranked
            .iter()
            .filter(|id| !self.tests.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        let failed = self
            .ranked
            .iter()
            .filter(|id| {
                self.tests
                    .get(*id)
                    .and_then(|row| row["outcome"].as_str())
                    .is_some_and(|outcome| matches!(outcome, "FAILED" | "ERROR"))
            })
            .cloned()
            .collect::<Vec<_>>();
        let status = if missing.is_empty()
            && (self.adapter == JunitAdapter::Ctest || self.unmapped.is_empty())
        {
            "COMPLETE"
        } else {
            "INCOMPLETE"
        };
        self.unranked_failures.sort();
        if self.adapter == JunitAdapter::Pytest {
            json!({"status": status, "tests": self.tests, "failed_nodeids": failed,
                "missing_nodeids": missing, "unmapped_cases": self.unmapped})
        } else {
            json!({"status": status, "tests": self.tests, "failed_nodeids": failed,
                "missing_nodeids": missing, "unmapped_cases": [],
                "unranked_failures": self.unranked_failures})
        }
    }
}

fn parse_junit<R: BufRead>(
    source: R,
    adapter: JunitAdapter,
    ranked: &[String],
) -> Result<Value, String> {
    let mut rows = JunitRows::new(adapter, ranked)?;
    let mut reader = Reader::from_reader(source);
    let mut buf = Vec::new();
    let mut cases = Vec::<XmlCase>::new();
    let mut depth = 0_usize;
    let mut saw_root = false;
    let mut root_closed = false;
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|error| error.to_string())?
        {
            Event::Start(start) => {
                if root_closed {
                    return Err("JUnit report has more than one document root".to_owned());
                }
                depth += 1;
                saw_root = true;
                if start.name().as_ref() == b"testcase" {
                    cases.push(xml_case(&start, &reader, depth)?);
                } else if let Some(case) = cases.last_mut() {
                    if depth == case.depth + 1 {
                        mark_child(case, start.name().as_ref());
                    }
                }
            }
            Event::Empty(start) => {
                if root_closed {
                    return Err("JUnit report has more than one document root".to_owned());
                }
                if depth == 0 {
                    root_closed = true;
                }
                saw_root = true;
                if start.name().as_ref() == b"testcase" {
                    rows.consume(xml_case(&start, &reader, depth + 1)?)?;
                } else if let Some(case) = cases.last_mut() {
                    if depth + 1 == case.depth + 1 {
                        mark_child(case, start.name().as_ref());
                    }
                }
            }
            Event::End(end) => {
                if depth == 0 {
                    return Err("JUnit report has an unmatched closing tag".to_owned());
                }
                if end.name().as_ref() == b"testcase" {
                    if let Some(case) = cases.pop() {
                        rows.consume(case)?;
                    }
                }
                depth -= 1;
                if depth == 0 {
                    root_closed = true;
                }
            }
            Event::DocType(_) => {
                return Err("DTD declarations are forbidden in JUnit reports".to_owned())
            }
            Event::Text(text) if depth == 0 && !text.iter().all(u8::is_ascii_whitespace) => {
                return Err("JUnit report contains text outside its document root".to_owned());
            }
            Event::GeneralRef(reference) => {
                if depth == 0 {
                    return Err("JUnit report contains text outside its document root".to_owned());
                }
                let name = reference.as_ref();
                let predefined =
                    [b"amp".as_slice(), b"lt", b"gt", b"quot", b"apos"].contains(&name);
                if !predefined
                    && reference
                        .resolve_char_ref()
                        .map_err(|error| error.to_string())?
                        .is_none()
                {
                    return Err("JUnit report contains an undefined entity reference".to_owned());
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if !saw_root || depth != 0 || !cases.is_empty() {
        return Err("JUnit report has no complete document root".to_owned());
    }
    Ok(rows.finish())
}

fn mark_child(case: &mut XmlCase, name: &[u8]) {
    match name {
        b"error" => case.error = true,
        b"failure" => case.failure = true,
        b"skipped" => case.skipped = true,
        _ => {}
    }
}

pub fn parse_junit_text(
    xml: &str,
    adapter: JunitAdapter,
    ranked: &[String],
) -> Result<Value, String> {
    if xml.len() as u64 > MAX_REPORT_BYTES {
        return Err(format!("JUnit report exceeds {MAX_REPORT_BYTES} bytes"));
    }
    parse_junit(Cursor::new(xml.as_bytes()), adapter, ranked)
}

pub fn parse_junit_file(
    path: &Path,
    adapter: JunitAdapter,
    ranked: &[String],
) -> Result<Value, String> {
    let label = format!("cannot parse {} JUnit report", adapter.label());
    let file = File::open(path).map_err(|error| format!("{label}: {error}"))?;
    let size = file
        .metadata()
        .map_err(|error| format!("{label}: {error}"))?
        .len();
    if size > MAX_REPORT_BYTES {
        return Err(format!("{label}: report exceeds {MAX_REPORT_BYTES} bytes"));
    }
    parse_junit(
        BufReader::new(file.take(MAX_REPORT_BYTES + 1)),
        adapter,
        ranked,
    )
    .map_err(|error| format!("{label}: {error}"))
}

fn libtest_line(line: &str) -> Option<(String, &'static str)> {
    if line.trim_start().starts_with('{') {
        let row: Value = serde_json::from_str(line).ok()?;
        if row.get("type")?.as_str()? != "test" {
            return None;
        }
        let outcome = match row.get("event")?.as_str()? {
            "ok" => "PASSED",
            "failed" => "FAILED",
            "ignored" => "SKIPPED",
            _ => return None,
        };
        return Some((row.get("name")?.as_str()?.to_owned(), outcome));
    }
    let mut fields = line.split_whitespace();
    if fields.next()? != "test" {
        return None;
    }
    let name = fields.next()?;
    if fields.next()? != "..." {
        return None;
    }
    let suffix = fields.next()?;
    for (token, outcome) in [
        ("ok", "PASSED"),
        ("FAILED", "FAILED"),
        ("ignored", "SKIPPED"),
    ] {
        if let Some(after) = suffix.strip_prefix(token) {
            if !after
                .chars()
                .next()
                .is_some_and(|ch| ch == '_' || ch.is_alphanumeric())
            {
                return Some((name.to_owned(), outcome));
            }
        }
    }
    None
}

pub fn parse_cargo_libtest(stdout: &str, ranked: &[String]) -> Result<Value, String> {
    let mut by_name = HashMap::new();
    for nodeid in ranked {
        by_name.insert(cargo_identity(nodeid)?, nodeid.clone());
    }
    if by_name.len() != ranked.len() {
        return Err(
            "ranked tests share a function name; libtest output cannot separate them".to_owned(),
        );
    }
    let mut tests = Map::<String, Value>::new();
    let mut printed_names = HashMap::<String, HashSet<String>>::new();
    let mut unranked_failures = Vec::new();
    for line in stdout.lines() {
        let Some((printed, outcome)) = libtest_line(line) else {
            continue;
        };
        let function = printed.rsplit("::").next().unwrap_or(&printed);
        let Some(nodeid) = by_name.get(function) else {
            if outcome == "FAILED" {
                unranked_failures.push(printed);
            }
            continue;
        };
        let row = tests
            .entry(nodeid.clone())
            .or_insert_with(|| json!({"outcome": outcome, "cases": 0}));
        if outcome == "FAILED" {
            row["outcome"] = json!("FAILED");
        }
        row["cases"] = json!(row["cases"].as_u64().unwrap_or(0) + 1);
        printed_names
            .entry(nodeid.clone())
            .or_default()
            .insert(printed);
    }
    let mut ambiguous = printed_names
        .into_iter()
        .filter_map(|(id, names)| (names.len() > 1).then_some(id))
        .collect::<Vec<_>>();
    ambiguous.sort();
    for nodeid in &ambiguous {
        tests.remove(nodeid);
    }
    let missing = ranked
        .iter()
        .filter(|id| !tests.contains_key(*id))
        .cloned()
        .collect::<Vec<_>>();
    unranked_failures.sort();
    Ok(
        json!({"status": if missing.is_empty() { "COMPLETE" } else { "INCOMPLETE" },
        "tests": tests, "missing_nodeids": missing, "ambiguous_nodeids": ambiguous,
        "unmapped_cases": [], "unranked_failures": unranked_failures}),
    )
}

/// Normalize runner reports before the native set-cover/value scorer consumes them.
/// The ordered mutation contracts determine output order, exactly as in a campaign.
pub fn collect_mutant_evidence(
    ranked: &[String],
    mutation_contracts: &[(String, String)],
    reports: &Value,
    outcomes: &Value,
) -> Vec<Value> {
    mutation_contracts
        .iter()
        .map(|(mutation_id, _)| {
            let report = reports.get(mutation_id).filter(|row| row.is_object());
            let tests = report
                .and_then(|row| row.get("tests"))
                .and_then(Value::as_object);
            let state = if report
                .and_then(|row| row.get("status"))
                .and_then(Value::as_str)
                != Some("COMPLETE")
            {
                "INCOMPLETE"
            } else if tests.is_none() {
                "NO_TEST_MAP"
            } else {
                "COMPLETE"
            };
            let killers = if state == "COMPLETE" {
                let report = report.expect("complete report exists");
                match report.get("failed_nodeids").and_then(Value::as_array) {
                    Some(failed) if failed.iter().all(Value::is_string) => {
                        let failed = failed
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<HashSet<_>>();
                        ranked
                            .iter()
                            .filter(|id| failed.contains(id.as_str()))
                            .cloned()
                            .collect::<Vec<_>>()
                    }
                    _ => ranked
                        .iter()
                        .filter(|id| {
                            tests
                                .and_then(|rows| rows.get(*id))
                                .and_then(Value::as_object)
                                .and_then(|row| row.get("outcome"))
                                .and_then(Value::as_str)
                                .is_some_and(|outcome| matches!(outcome, "FAILED" | "ERROR"))
                        })
                        .cloned()
                        .collect::<Vec<_>>(),
                }
            } else {
                Vec::new()
            };
            json!({"mutation_id": mutation_id,
            "outcome": outcomes.get(mutation_id).cloned().unwrap_or(Value::Null),
            "report_state": state, "killers": killers})
        })
        .collect()
}
