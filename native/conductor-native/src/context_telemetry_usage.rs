//! Actual provider usage ledger. Tool byte estimates never become billed tokens.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

const COUNTERS: [&str; 6] = [
    "input_tokens",
    "output_tokens",
    "cached_input_tokens",
    "cache_creation_input_tokens",
    "reasoning_tokens",
    "total_tokens",
];

/// Collect routing labels only. Request/task identities are hashed, contents absent.
pub fn labels(payload: &Value) -> Map<String, Value> {
    let mut result = Map::new();
    for (output, paths) in [
        (
            "request_id_hash",
            &[
                "request_id",
                "response_id",
                "response.id",
                "metadata.request_id",
            ][..],
        ),
        ("task_id_hash", &["task_id", "metadata.task_id"][..]),
        ("model", &["model", "response.model", "metadata.model"][..]),
        (
            "task_outcome",
            &["task_outcome", "metadata.task_outcome"][..],
        ),
    ] {
        if let Some(text) = paths.iter().find_map(|path| {
            path.split('.')
                .try_fold(payload, |value, key| value.get(key))?
                .as_str()
                .filter(|text| !text.is_empty())
        }) {
            if output.ends_with("_hash") {
                result.insert(
                    output.into(),
                    json!(format!("{:x}", Sha256::digest(text.as_bytes()))),
                );
            } else if output != "task_outcome"
                || matches!(
                    text,
                    "validated" | "completed" | "failed" | "cancelled" | "running"
                )
            {
                result.insert(
                    output.into(),
                    json!(text.chars().take(128).collect::<String>()),
                );
            }
        }
    }
    for name in ["model_latency_ms", "time_to_first_token_ms"] {
        if let Some(value) = payload
            .get(name)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
        {
            result.insert(name.into(), json!(value));
        }
    }
    result
}

#[derive(Default, Clone)]
struct Request {
    provider: String,
    model: String,
    task: String,
    counters: [Option<u64>; 6],
    input_excludes_cache: bool,
    latency: Option<f64>,
    ttft: Option<f64>,
}

fn label(item: &Map<String, Value>, name: &str) -> String {
    item.get(name)
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned()
}

impl Request {
    fn from_item(item: &Map<String, Value>) -> Self {
        let counters = COUNTERS.map(|name| item.get(name).and_then(Value::as_u64));
        let input_excludes_cache = item
            .get("native_usage_fields")
            .and_then(Value::as_array)
            .is_some_and(|fields| {
                fields.iter().any(|field| {
                    matches!(
                        field.as_str(),
                        Some("cache_read_input_tokens" | "cache_creation_input_tokens")
                    )
                })
            });
        Self {
            provider: label(item, "provider"),
            model: label(item, "model"),
            task: label(item, "task_id_hash"),
            counters,
            input_excludes_cache,
            latency: item
                .get("model_latency_ms")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v >= 0.0),
            ttft: item
                .get("time_to_first_token_ms")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v >= 0.0),
        }
    }

    fn merge(&mut self, other: Self) -> bool {
        let changed = self
            .counters
            .iter()
            .zip(other.counters.iter())
            .any(|(old, new)| old.is_some() && new.is_some() && old != new);
        for (old, new) in self.counters.iter_mut().zip(other.counters) {
            *old = match (*old, new) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
        }
        if other.model != "unknown" {
            self.model = other.model;
        }
        if other.task != "unknown" {
            self.task = other.task;
        }
        self.input_excludes_cache |= other.input_excludes_cache;
        self.latency = other.latency.or(self.latency);
        self.ttft = other.ttft.or(self.ttft);
        changed
    }

    fn total_input(&self) -> Option<u64> {
        self.counters[0].map(|input| {
            if self.input_excludes_cache {
                input
                    .saturating_add(self.counters[2].unwrap_or(0))
                    .saturating_add(self.counters[3].unwrap_or(0))
            } else {
                input
            }
        })
    }
}

#[derive(Default)]
pub struct UsageLedger {
    keyed: BTreeMap<(String, String, String), Request>,
    unkeyed: Vec<Request>,
    tasks: BTreeMap<String, String>,
    events: u64,
    usage_events: u64,
    duplicate_events: u64,
    counter_updates: u64,
    observed_request_ids: BTreeSet<(String, String, String)>,
}

impl UsageLedger {
    pub fn add(&mut self, item: &Map<String, Value>) {
        self.events += 1;
        let key = item
            .get("request_id_hash")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(|id| {
                (
                    label(item, "provider"),
                    label(item, "session_id"),
                    id.to_owned(),
                )
            });
        if let Some(key) = &key {
            self.observed_request_ids.insert(key.clone());
        }
        if let (Some(task), Some(outcome)) = (
            item.get("task_id_hash").and_then(Value::as_str),
            item.get("task_outcome").and_then(Value::as_str),
        ) {
            self.tasks.insert(task.to_owned(), outcome.to_owned());
        }
        let request = Request::from_item(item);
        if !request.counters.iter().any(Option::is_some) {
            return;
        }
        self.usage_events += 1;
        let Some(key) = key else {
            self.unkeyed.push(request);
            return;
        };
        if let Some(existing) = self.keyed.get_mut(&key) {
            self.duplicate_events += 1;
            self.counter_updates += u64::from(existing.merge(request));
        } else {
            self.keyed.insert(key, request);
        }
    }

    pub fn finish(self) -> Value {
        let requests: Vec<&Request> = self.keyed.values().chain(self.unkeyed.iter()).collect();
        let tokens = totals(&requests);
        let validated = self
            .tasks
            .values()
            .filter(|outcome| outcome.as_str() == "validated")
            .count();
        let mut models: BTreeMap<(&str, &str), Vec<&Request>> = BTreeMap::new();
        let mut tasks: BTreeMap<&str, Vec<&Request>> = BTreeMap::new();
        for request in &requests {
            models
                .entry((&request.provider, &request.model))
                .or_default()
                .push(request);
            if request.task != "unknown" {
                tasks.entry(&request.task).or_default().push(request);
            }
        }
        json!({
            "usage_events": self.usage_events, "events_without_usage": self.events - self.usage_events,
            "keyed_requests": self.keyed.len(), "unkeyed_usage_events": self.unkeyed.len(),
            "deduplicated_events": self.duplicate_events, "counter_updates": self.counter_updates,
            "counting": "keyed request counters merged by maximum; unkeyed records cannot be deduplicated",
            "actual_usage_event_fraction": if self.events == 0 { 0.0 } else { self.usage_events as f64 / self.events as f64 },
            "observed_request_identities": self.observed_request_ids.len(),
            "request_identity_coverage": if self.observed_request_ids.is_empty() { None } else { Some(self.keyed.len() as f64 / self.observed_request_ids.len() as f64) },
            "complete_io_keyed_requests": self.keyed.values().filter(|r| r.counters[0].is_some() && r.counters[1].is_some()).count(),
            "totals": tokens,
            "by_model": models.into_iter().map(|((provider, model), rows)| json!({"provider":provider,"model":model,"usage":totals(&rows)})).collect::<Vec<_>>(),
            "by_task": tasks.into_iter().map(|(task, rows)| json!({"task_id_hash":task,"outcome":self.tasks.get(task),"usage":totals(&rows)})).collect::<Vec<_>>(),
            "reported_validated_tasks": validated, "task_outcome_source": "caller-reported; not gate evidence",
            "tokens_per_reported_validated_task": if validated > 0 && self.unkeyed.is_empty() && requests.iter().all(|r| r.counters[0].is_some() && r.counters[1].is_some()) { tokens["total_input_tokens"].as_u64().zip(tokens["output_tokens"].as_u64()).map(|(input, output)| input.saturating_add(output) as f64 / validated as f64) } else { None },
        })
    }
}

fn sum_known(requests: &[&Request], select: impl Fn(&Request) -> Option<u64>) -> Option<u64> {
    let counts: Vec<_> = requests
        .iter()
        .filter_map(|request| select(request))
        .collect();
    if counts.is_empty() {
        None
    } else {
        Some(counts.into_iter().fold(0u64, u64::saturating_add))
    }
}

fn totals(requests: &[&Request]) -> Value {
    let mut result = Map::new();
    result.insert("requests".into(), json!(requests.len()));
    for (index, name) in COUNTERS.iter().enumerate() {
        let name = if *name == "total_tokens" {
            "reported_total_tokens"
        } else {
            name
        };
        result.insert(
            name.into(),
            json!(sum_known(requests, |request| request.counters[index])),
        );
        result.insert(
            format!("{name}_coverage"),
            json!(requests
                .iter()
                .filter(|r| r.counters[index].is_some())
                .count()),
        );
    }
    let input = sum_known(requests, Request::total_input);
    result.insert("total_input_tokens".into(), json!(input));
    result.insert(
        "uncached_input_tokens".into(),
        json!(sum_known(requests, |request| request
            .total_input()
            .zip(request.counters[2])
            .map(|(input, cached)| input
                .saturating_sub(cached)
                .saturating_sub(request.counters[3].unwrap_or(0))))),
    );
    let paired: Vec<_> = requests
        .iter()
        .filter(|r| r.total_input().is_some() && r.counters[2].is_some())
        .copied()
        .collect();
    result.insert("cache_ratio_requests".into(), json!(paired.len()));
    result.insert(
        "cache_read_ratio".into(),
        json!(sum_known(&paired, Request::total_input)
            .filter(|input| *input > 0)
            .zip(sum_known(&paired, |r| r.counters[2]))
            .map(|(input, cached)| cached as f64 / input as f64)),
    );
    result.insert(
        "latency_ms".into(),
        distribution(requests.iter().filter_map(|r| r.latency).collect()),
    );
    result.insert(
        "time_to_first_token_ms".into(),
        distribution(requests.iter().filter_map(|r| r.ttft).collect()),
    );
    Value::Object(result)
}

fn distribution(mut samples: Vec<f64>) -> Value {
    samples.sort_by(f64::total_cmp);
    let percentile = |fraction: f64| {
        samples
            .get(((samples.len().saturating_sub(1)) as f64 * fraction).ceil() as usize)
            .copied()
    };
    json!({"count":samples.len(),"p50":percentile(0.5),"p95":percentile(0.95)})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_dedup_does_not_double_count_cached_or_reasoning_subsets() {
        let mut ledger = UsageLedger::default();
        let row = json!({"provider":"openai","session_id":"s","request_id_hash":"r","input_tokens":100,"output_tokens":30,"cached_input_tokens":60,"reasoning_tokens":20});
        ledger.add(row.as_object().unwrap());
        ledger.add(row.as_object().unwrap());
        let result = ledger.finish();
        assert_eq!(result["totals"]["total_input_tokens"], 100);
        assert_eq!(result["totals"]["output_tokens"], 30);
        assert_eq!(result["totals"]["uncached_input_tokens"], 40);
        assert_eq!(result["deduplicated_events"], 1);
        assert!(result["tokens_per_reported_validated_task"].is_null());
    }

    #[test]
    fn anthropic_cache_accounting_missing_counters_and_unkeyed_coverage_are_explicit() {
        let mut ledger = UsageLedger::default();
        let row = json!({"input_tokens":10,"cached_input_tokens":50,"cache_creation_input_tokens":20,"native_usage_fields":["input_tokens","cache_read_input_tokens","cache_creation_input_tokens"]});
        ledger.add(row.as_object().unwrap());
        let result = ledger.finish();
        assert_eq!(result["totals"]["total_input_tokens"], 80);
        assert_eq!(result["totals"]["uncached_input_tokens"], 10);
        assert!(result["totals"]["output_tokens"].is_null());
        assert_eq!(result["unkeyed_usage_events"], 1);
    }

    #[test]
    fn routing_identity_is_hashed_and_does_not_capture_content() {
        let projected = labels(
            &json!({"request_id":"private-request","task_id":"private-task","model":"example","tool_output":"SECRET"}),
        );
        assert_eq!(projected["request_id_hash"].as_str().unwrap().len(), 64);
        assert!(!serde_json::to_string(&projected)
            .unwrap()
            .contains("private"));
        assert!(!projected.contains_key("tool_output"));
    }

    #[test]
    fn total_only_usage_is_kept_without_inferred_input_or_output() {
        let mut ledger = UsageLedger::default();
        let row = json!({"provider":"openai", "request_id_hash":"r", "total_tokens":73});
        ledger.add(row.as_object().unwrap());
        ledger.add(row.as_object().unwrap());
        ledger.add(json!({"hook_context_bytes":1000}).as_object().unwrap());
        let result = ledger.finish();
        assert_eq!(result["usage_events"], 2);
        assert_eq!(result["totals"]["reported_total_tokens"], 73);
        assert!(result["totals"]["input_tokens"].is_null());
        assert!(result["totals"]["output_tokens"].is_null());
        assert_eq!(result["request_identity_coverage"], 1.0);
        assert_eq!(result["complete_io_keyed_requests"], 0);
    }
}
