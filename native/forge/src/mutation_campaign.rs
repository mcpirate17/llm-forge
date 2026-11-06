//! One generated-engine manifest, typed: what a run may execute, the bytes it
//! pins and the survivor ratchet it answers to. The loader keeps the wording of
//! `conductor.mutation_engine_generated`, whose callers parse these refusals.

use crate::instant::{isoformat_utc, now};
use crate::json_canon::python_pretty;
use crate::mutation_write::atomic_write;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const GENERATED_ENGINES: [&str; 3] = ["cargo-mutants", "fest", "mull"];
pub const KILLED: &str = "KILLED";
pub const SURVIVED: &str = "SURVIVED";
pub const TIMED_OUT: &str = "TIMED_OUT";
pub const NO_COVERAGE: &str = "NO_COVERAGE";
pub const UNVIABLE: &str = "UNVIABLE";
pub const ERROR: &str = "ERROR";
pub const OUTCOMES: [&str; 6] = [KILLED, SURVIVED, TIMED_OUT, NO_COVERAGE, UNVIABLE, ERROR];

const MUTANT_TIMEOUT_BASELINE_MULTIPLIER: f64 = 3.0;
const MUTANT_TIMEOUT_FLOOR_SECONDS: i64 = 60;

/// Why a run stopped before it could be scored.
#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    /// A contract was not met; the run says so and exits 4.
    Refused(String),
    /// The operator ended the run with this signal.
    Cancelled(i32),
}

impl From<String> for Stop {
    fn from(message: String) -> Self {
        Self::Refused(message)
    }
}

impl From<&str> for Stop {
    fn from(message: &str) -> Self {
        Self::Refused(message.to_string())
    }
}

pub type Step<T> = Result<T, Stop>;

/// A Python `repr` of `text`, for messages shared with the Python runner.
pub fn py_repr(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// A Python `repr` of a list of strings.
pub fn py_list<I, S>(items: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let rendered: Vec<String> = items.into_iter().map(|s| py_repr(s.as_ref())).collect();
    format!("[{}]", rendered.join(", "))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn sha256_path(path: &Path) -> Result<String, String> {
    fs::read(path)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Dict,
    List,
    Int,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Text => "str",
            Self::Dict => "dict",
            Self::List => "list",
            Self::Int => "int",
        }
    }

    fn admits(self, value: &Value) -> bool {
        match self {
            Self::Text => value.is_string(),
            Self::Dict => value.is_object(),
            Self::List => value.is_array(),
            Self::Int => value.is_i64() || value.is_u64(),
        }
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) if number.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn required<'a>(
    payload: &'a Map<String, Value>,
    key: &str,
    kind: Kind,
) -> Result<&'a Value, String> {
    let value = payload
        .get(key)
        .ok_or_else(|| format!("manifest is missing required key {key}", key = py_repr(key)))?;
    if kind.admits(value) {
        return Ok(value);
    }
    Err(format!(
        "{} must be {}, got {}",
        py_repr(key),
        kind.name(),
        type_name(value)
    ))
}

fn required_text(payload: &Map<String, Value>, key: &str) -> Result<String, String> {
    Ok(required(payload, key, Kind::Text)?
        .as_str()
        .unwrap_or_default()
        .to_string())
}

fn text_list(value: &Value, key: &str) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{} must be list, got {}", py_repr(key), type_name(value)))?
        .iter()
        .map(|item| {
            item.as_str().map(str::to_string).ok_or_else(|| {
                format!(
                    "{} must hold strings, got {}",
                    py_repr(key),
                    type_name(item)
                )
            })
        })
        .collect()
}

fn text_map(value: &Value, key: &str) -> Result<BTreeMap<String, String>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{} must be dict, got {}", py_repr(key), type_name(value)))?
        .iter()
        .map(|(name, item)| {
            item.as_str()
                .map(|text| (name.clone(), text.to_string()))
                .ok_or_else(|| {
                    format!(
                        "{} must hold strings, got {}",
                        py_repr(key),
                        type_name(item)
                    )
                })
        })
        .collect()
}

fn optional_list(payload: &Map<String, Value>, key: &str) -> Result<Vec<String>, String> {
    payload
        .get(key)
        .map_or(Ok(Vec::new()), |value| text_list(value, key))
}

fn optional_int(payload: &Map<String, Value>, key: &str) -> Result<Option<i64>, String> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("{} must be int, got {}", py_repr(key), type_name(value))),
    }
}

/// Campaign identifiers become output filenames, never relative paths.
pub(crate) fn validate_campaign_id(id: &str) -> Result<(), String> {
    if id.len() > 200
        || !id.starts_with(char::is_alphanumeric)
        || !id
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err("campaign_id must be a single filename component of at most 200 bytes, starting with a letter or digit and containing only letters, digits, '.', '_' or '-'".into());
    }
    Ok(())
}

/// A manifest for an engine that generates its own mutants.
#[derive(Clone, Debug)]
pub struct Campaign {
    pub manifest_path: PathBuf,
    pub manifest_sha256: String,
    pub campaign_id: String,
    pub title: String,
    pub language: String,
    pub mutation_engine: String,
    pub source: Vec<String>,
    pub exclude: Vec<String>,
    pub operators: Vec<String>,
    pub options: Map<String, Value>,
    pub seed: i64,
    pub jobs: i64,
    pub mutant_timeout_seconds: Option<i64>,
    pub run_timeout_seconds: i64,
    pub test_argv: Vec<String>,
    pub source_sha256: BTreeMap<String, String>,
    pub test_sha256: BTreeMap<String, String>,
    pub survivor_baseline: Vec<String>,
    pub survivor_baseline_recorded: bool,
    pub environment: BTreeMap<String, String>,
    pub generated_value: Option<Value>,
}

impl Campaign {
    /// Load and validate one generated-engine manifest.
    pub fn load(path: &Path) -> Result<Self, String> {
        let read = |error: String| format!("cannot read manifest {}: {error}", path.display());
        let text = fs::read_to_string(path).map_err(|error| read(error.to_string()))?;
        let payload: Value =
            serde_json::from_str(&text).map_err(|error| read(error.to_string()))?;
        let payload = payload
            .as_object()
            .ok_or_else(|| "manifest root must be a JSON object".to_string())?;
        let campaign = Self::from_payload(path, payload)?;
        campaign.require_declared_scope()?;
        Ok(campaign)
    }

    fn from_payload(path: &Path, payload: &Map<String, Value>) -> Result<Self, String> {
        let campaign_id = required_text(payload, "campaign_id")?;
        validate_campaign_id(&campaign_id)?;
        let title = required_text(payload, "title")?;
        let language = required_text(payload, "language")?;
        let mutation_engine = required_text(payload, "mutation_engine")?;
        if !GENERATED_ENGINES.contains(&mutation_engine.as_str()) {
            return Err(format!(
                "{} is not a generated engine; known: {}",
                py_repr(&mutation_engine),
                py_list(GENERATED_ENGINES)
            ));
        }
        let generator = required(payload, "generator", Kind::Dict)?
            .as_object()
            .expect("kind checked");
        let profile = conductor_native::mutation_evidence::execution_profile(
            generator,
            &language,
            &mutation_engine,
        )?;
        if profile.is_some() && payload.contains_key("generated_value") {
            return Err("workspace execution profile cannot carry a value contract".into());
        }
        let source = text_list(required(generator, "source", Kind::List)?, "source")?;
        let options = generator
            .get("options")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let baseline = optional_list(payload, "survivor_baseline")?;
        let recorded_by_default = !baseline.is_empty();
        let environment = payload
            .get("environment")
            .map_or(Ok(BTreeMap::new()), |value| text_map(value, "environment"))?;
        Ok(Self {
            manifest_path: path.to_path_buf(),
            manifest_sha256: sha256_path(path)?,
            campaign_id,
            title,
            language,
            mutation_engine,
            source,
            exclude: optional_list(generator, "exclude")?,
            operators: optional_list(generator, "operators")?,
            options,
            seed: optional_int(generator, "seed")?.unwrap_or(0),
            jobs: optional_int(generator, "jobs")?.unwrap_or(1),
            mutant_timeout_seconds: optional_int(generator, "mutant_timeout_seconds")?,
            run_timeout_seconds: required(generator, "run_timeout_seconds", Kind::Int)?
                .as_i64()
                .ok_or_else(|| "'run_timeout_seconds' does not fit an integer".to_string())?,
            test_argv: text_list(required(payload, "test_argv", Kind::List)?, "test_argv")?,
            source_sha256: text_map(
                required(payload, "source_sha256", Kind::Dict)?,
                "source_sha256",
            )?,
            test_sha256: text_map(required(payload, "test_sha256", Kind::Dict)?, "test_sha256")?,
            survivor_baseline: {
                let mut sorted = baseline;
                sorted.sort();
                sorted
            },
            survivor_baseline_recorded: payload
                .get("survivor_baseline_recorded")
                .map_or(recorded_by_default, python_truth),
            environment,
            generated_value: payload.get("generated_value").cloned(),
        })
    }

    fn require_declared_scope(&self) -> Result<(), String> {
        if self.source.is_empty() {
            return Err("generator.source must name at least one glob".into());
        }
        if self.source_sha256.is_empty() {
            return Err("source_sha256 must pin every mutated file".into());
        }
        if self.test_sha256.is_empty() {
            return Err("test_sha256 must pin every test file that ran".into());
        }
        let named: BTreeSet<&String> = self
            .test_sha256
            .keys()
            .filter(|path| self.test_argv.contains(path))
            .collect();
        if !named.is_empty() && named.len() != self.test_sha256.len() {
            let missing = self.test_sha256.keys().filter(|path| !named.contains(path));
            return Err(format!(
                "test_argv names some declared tests but not {}; a campaign may only \
                 vouch for tests it actually executed",
                py_list(missing)
            ));
        }
        Ok(())
    }

    /// `generator.options.<key>` as text, when the manifest sets a non-empty one.
    pub fn option_text(&self, key: &str) -> Option<String> {
        match self.options.get(key) {
            Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
            Some(Value::Number(number)) => Some(number.to_string()),
            _ => None,
        }
    }

    /// Pin the per-mutant bound: the manifest's, else three baselines, at least 60s.
    pub fn resolve_mutant_timeout(&mut self, baseline_seconds: f64) -> i64 {
        if let Some(pinned) = self.mutant_timeout_seconds {
            return pinned;
        }
        let scaled = (MUTANT_TIMEOUT_BASELINE_MULTIPLIER * baseline_seconds).ceil() as i64;
        let derived = scaled.max(MUTANT_TIMEOUT_FLOOR_SECONDS);
        self.mutant_timeout_seconds = Some(derived);
        derived
    }

    /// The bound `resolve_mutant_timeout` fixed once the baseline ran.
    pub fn mutant_timeout(&self) -> Result<i64, String> {
        self.mutant_timeout_seconds
            .ok_or_else(|| "the per-mutant bound was read before the baseline set it".to_string())
    }
}

fn python_truth(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|float| float != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(items) => !items.is_empty(),
    }
}

pub use conductor_native::mutation_generated_identity::identify;

fn drift_into(
    pins: &BTreeMap<String, String>,
    root: &Path,
    drifted: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    for (relative, expected) in pins {
        let target = root.join(relative);
        if !target.is_file() {
            drifted.insert(relative.clone(), "absent".into());
            continue;
        }
        let actual = sha256_path(&target)?;
        if &actual != expected {
            drifted.insert(relative.clone(), actual);
        }
    }
    Ok(())
}

/// Files under `root` whose bytes are not what the manifest pinned: sources and tests.
pub fn drift(campaign: &Campaign, root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut drifted = BTreeMap::new();
    drift_into(&campaign.source_sha256, root, &mut drifted)?;
    drift_into(&campaign.test_sha256, root, &mut drifted)?;
    Ok(drifted)
}

/// Refuse a snapshot whose pinned bytes moved before any engine ran.
pub fn require_no_drift(campaign: &Campaign, root: &Path) -> Result<(), String> {
    let drifted = drift(campaign, root)?;
    if drifted.is_empty() {
        return Ok(());
    }
    Err(format!("snapshot source hashes drifted: {drifted:?}"))
}

/// Resolve a bare `python` argv[0] to the interpreter the receipt pins.
pub fn pinned(argv: &[String], interpreter: &str) -> Vec<String> {
    let mut resolved = argv.to_vec();
    if matches!(
        resolved.first().map(String::as_str),
        Some("python" | "python3")
    ) {
        resolved[0] = interpreter.to_string();
    }
    resolved
}

/// Write a first run's survivor set into its own manifest, exactly once.
///
/// The tool records it, never an agent, so the survivors are whatever the engine
/// found. Returns whether the manifest changed; the caller re-scores if so.
pub fn record_survivor_baseline(
    campaign: &mut Campaign,
    survivors: &[String],
) -> Result<bool, String> {
    if campaign.survivor_baseline_recorded {
        return Ok(false);
    }
    let path = campaign.manifest_path.clone();
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read manifest {}: {error}", path.display()))?;
    let mut payload: Map<String, Value> = serde_json::from_str(&text)
        .map_err(|error| format!("cannot read manifest {}: {error}", path.display()))?;
    payload.insert("survivor_baseline".into(), Value::from(survivors.to_vec()));
    payload.insert("survivor_baseline_recorded".into(), Value::Bool(true));
    payload.insert(
        "survivor_baseline_recorded_at".into(),
        Value::from(isoformat_utc(now())),
    );
    payload.remove("survivor_baseline_note");
    let body = python_pretty(Value::Object(payload)).map_err(|error| error.to_string())?;
    atomic_write(&path, &body)?;
    let mut sorted = survivors.to_vec();
    sorted.sort();
    campaign.survivor_baseline = sorted;
    campaign.survivor_baseline_recorded = true;
    campaign.manifest_sha256 = sha256_path(&path)?;
    Ok(true)
}

#[cfg(test)]
#[path = "mutation_campaign_tests.rs"]
mod tests;
