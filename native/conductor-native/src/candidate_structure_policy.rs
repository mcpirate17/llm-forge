//! Final structure-audit policy over admitted native facts.
//!
//! Diagnostic schema: expression displays retain captured source spelling. Lock
//! and default equality uses the parent's location-free canonical AST keys, never
//! diagnostic strings. Equivalent quote/spacing choices therefore cannot create
//! or hide conflicts. No filesystem, interpreter or ambient repository access.
use super::{AbstractionFact, ConfigFact, FileFacts, LockOrderFact};
use crate::candidate_python::Document;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const MAX_FILES: usize = 4096;
const MAX_SOURCE: usize = 1024 * 1024;
const MAX_TOTAL: usize = 32 * 1024 * 1024;
const MAX_NODES: usize = 1_000_000;
const MAX_FINDINGS: usize = 4096;
const MAX_OUTPUT: usize = 32 * 1024 * 1024;
const CHECK: &str = "structure-audit";

struct Site<'a, T> {
    path: &'a str,
    fact: &'a T,
}
struct ConfigGroup<'a> {
    sites: Vec<Site<'a, ConfigFact>>,
    defaults: BTreeMap<&'a str, &'a str>,
}
#[derive(Default)]
struct Index<'a> {
    orders: BTreeMap<(&'a str, &'a str), Vec<Site<'a, LockOrderFact>>>,
    declared: BTreeMap<&'a str, (&'a str, &'a AbstractionFact)>,
    subclasses: BTreeMap<&'a str, BTreeSet<&'a str>>,
    configs: BTreeMap<&'a str, ConfigGroup<'a>>,
}

fn array<'a>(value: &'a Value, name: &str) -> Result<&'a [Value], String> {
    value
        .get(name)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("structure-audit requires array {name}"))
}
fn path(value: &Value) -> Result<&str, String> {
    let path = value
        .as_str()
        .ok_or("structure-audit requires string path")?;
    if path.is_empty()
        || path.len() > 4096
        || path.contains('\0')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("structure-audit requires bounded canonical relative paths".into());
    }
    Ok(path)
}
fn inputs(payload: &Value) -> Result<(BTreeMap<&str, &str>, BTreeSet<&str>), String> {
    let files = array(payload, "files")?;
    let changed_values = array(payload, "changed")?;
    if files.len() > MAX_FILES || changed_values.len() > MAX_FILES {
        return Err("structure-audit file count exceeds 4096".into());
    }
    let mut changed = BTreeSet::new();
    for value in changed_values {
        changed.insert(path(value)?);
    }
    let mut records = BTreeMap::new();
    let mut total = 0usize;
    for file in files {
        let name = path(
            file.get("path")
                .ok_or("structure-audit file requires path")?,
        )?;
        let source = file
            .get("text")
            .and_then(Value::as_str)
            .ok_or("structure-audit file requires string text")?;
        total = total
            .checked_add(source.len())
            .ok_or("structure-audit source size overflow")?;
        if source.len() > MAX_SOURCE || total > MAX_TOTAL {
            return Err("structure-audit source exceeds 1 MiB/file or 32 MiB aggregate".into());
        }
        if records.insert(name, source).is_some() {
            return Err(format!("structure-audit repeated inventory path: {name}"));
        }
    }
    if changed.iter().any(|path| !records.contains_key(path)) {
        return Err("structure-audit changed inventory is missing source".into());
    }
    Ok((records, changed))
}

fn collect(
    records: &BTreeMap<&str, &str>,
    changed: &BTreeSet<&str>,
) -> Result<Vec<FileFacts>, String> {
    let mut nodes = 0usize;
    let mut canonical_bytes = 0usize;
    let mut facts = Vec::with_capacity(records.len());
    for (path, source) in records {
        let document = Document::parse(source)
            .map_err(|error| format!("incomplete structure-audit peer parse {path}: {error}"))?;
        nodes = nodes
            .checked_add(document.nodes())
            .ok_or("structure-audit node count overflow")?;
        if nodes > MAX_NODES {
            return Err("structure-audit aggregate AST exceeds 1000000 nodes".into());
        }
        let file = super::analyze_document((*path).to_owned(), &document, changed.contains(path))?;
        canonical_bytes = canonical_bytes
            .checked_add(file.canonical_bytes)
            .ok_or("structure-audit canonicalization size overflow")?;
        if canonical_bytes > MAX_TOTAL {
            return Err("structure-audit canonicalization exceeds 32 MiB".into());
        }
        facts.push(file);
    }
    Ok(facts)
}

fn index(facts: &[FileFacts]) -> Index<'_> {
    let mut index = Index::default();
    for file in facts {
        for fact in &file.lock_orders {
            index
                .orders
                .entry((&fact.outer_key, &fact.inner_key))
                .or_default()
                .push(Site {
                    path: &file.path,
                    fact,
                });
        }
        for fact in &file.abstractions {
            // Legacy policy overwrites earlier declarations in sorted path order.
            index.declared.insert(&fact.name, (&file.path, fact));
        }
        for fact in &file.subclasses {
            index
                .subclasses
                .entry(&fact.base)
                .or_default()
                .insert(&fact.implementation);
        }
        for fact in &file.configs {
            let group = index
                .configs
                .entry(&fact.key)
                .or_insert_with(|| ConfigGroup {
                    sites: Vec::new(),
                    defaults: BTreeMap::new(),
                });
            group
                .defaults
                .entry(&fact.default_key)
                .or_insert(&fact.default);
            group.sites.push(Site {
                path: &file.path,
                fact,
            });
        }
    }
    index
}

struct Budget {
    findings: usize,
    bytes: usize,
}
impl Budget {
    fn new(files: &BTreeSet<&str>) -> Result<Self, String> {
        let bytes = crate::candidate_cargo_audit::serialized_size(files, MAX_OUTPUT)?;
        Ok(Self {
            findings: 0,
            bytes: bytes + 1024,
        })
    }
    // Each string appears in at most message + evidence, JSON escaping costs at
    // most six bytes per UTF-8 input byte. Reserve before cloning evidence or
    // joining amplified peer/default lists; keys and punctuation get fixed slack.
    fn reserve(&mut self, raw_bytes: usize) -> Result<(), String> {
        self.findings += 1;
        self.bytes = raw_bytes
            .checked_mul(12)
            .and_then(|n| n.checked_add(1024))
            .and_then(|n| self.bytes.checked_add(n))
            .ok_or("structure-audit output size overflow")?;
        if self.findings > MAX_FINDINGS || self.bytes > MAX_OUTPUT {
            return Err("structure-audit exceeds 4096 findings or 32 MiB evidence output".into());
        }
        Ok(())
    }
}
fn finding(
    rule: &str,
    severity: &str,
    path: &str,
    line: usize,
    message: String,
    help: &str,
    evidence: Value,
) -> Value {
    json!({"check_id":CHECK,"rule_id":rule,"severity":severity,"path":path,"line":line,
        "message":message,"help":help,"evidence":evidence})
}

fn lifecycle(
    facts: &[FileFacts],
    changed: &BTreeSet<&str>,
    budget: &mut Budget,
    findings: &mut Vec<Value>,
    emit: bool,
) -> Result<(), String> {
    for file in facts {
        if !changed.contains(file.path.as_str()) {
            continue;
        }
        for fact in &file.unbounded {
            budget.reserve(file.path.len() + fact.name.len())?;
            if !emit {
                continue;
            }
            findings.push(finding("unbounded-module-cache", "medium", &file.path, fact.growth_line,
                format!("module-level '{}' (defined line {}) grows at runtime with no eviction, bound or maximum size",
                    fact.name, fact.defined_line),
                "Bound the container (maxsize/maxlen) or add an explicit eviction path.",
                json!({"container":fact.name,"defined_line":fact.defined_line})));
        }
        for fact in &file.leaks {
            budget.reserve(
                file.path.len()
                    + fact.resource.len()
                    + fact.function.len()
                    + fact.acquire.len()
                    + fact.release.len(),
            )?;
            if !emit {
                continue;
            }
            let (rule, severity) = if fact.owned {
                ("cleanup-not-on-failure-path", "high")
            } else {
                ("connection-close-not-on-failure-path", "medium")
            };
            findings.push(finding(rule, severity, &file.path, fact.line,
                format!("{}.{}() in '{}' is released by {}.{}() only on the success path; an exception raised in between leaks it",
                    fact.resource, fact.acquire, fact.function, fact.resource, fact.release),
                "Use a with-statement, or move the release into a try/finally.",
                json!({"resource":fact.resource,"acquire":fact.acquire,"release":fact.release})));
        }
    }
    Ok(())
}

fn locks(
    index: &Index<'_>,
    changed: &BTreeSet<&str>,
    budget: &mut Budget,
    findings: &mut Vec<Value>,
    emit: bool,
) -> Result<(), String> {
    for ((outer, inner), sites) in &index.orders {
        let Some(opposite) = index.orders.get(&(*inner, *outer)) else {
            continue;
        };
        let opposite_bytes = opposite
            .iter()
            .take(3)
            .map(|site| site.path.len() + 32)
            .sum::<usize>();
        for site in sites {
            if !changed.contains(site.path) {
                continue;
            }
            let fact = site.fact;
            budget
                .reserve(site.path.len() + fact.outer.len() + fact.inner.len() + opposite_bytes)?;
            if !emit {
                continue;
            }
            let where_ = opposite
                .iter()
                .take(3)
                .map(|site| format!("{}:{}", site.path, site.fact.line))
                .collect::<Vec<_>>()
                .join(", ");
            findings.push(finding("inconsistent-lock-order", "high", site.path, fact.line,
                format!("takes {} before {}, but {where_} takes them in the opposite order; the two orderings can deadlock",fact.outer,fact.inner),
                "Impose one global lock order, or take both under a single guard.",
                json!({"outer":fact.outer,"inner":fact.inner,"opposite_sites":where_})));
        }
    }
    Ok(())
}

fn abstractions(
    index: &Index<'_>,
    changed: &BTreeSet<&str>,
    budget: &mut Budget,
    findings: &mut Vec<Value>,
    emit: bool,
) -> Result<(), String> {
    for (name, (path, fact)) in &index.declared {
        if !changed.contains(path) || fact.methods < 2 {
            continue;
        }
        let implementations = index.subclasses.get(name);
        let count = implementations.map_or(0, BTreeSet::len);
        if count > 1 {
            continue;
        }
        let implementation_bytes = implementations
            .into_iter()
            .flatten()
            .map(|s| s.len())
            .sum::<usize>();
        budget.reserve(path.len() + name.len() + implementation_bytes)?;
        if !emit {
            continue;
        }
        let implementations = implementations
            .into_iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        findings.push(finding("single-implementation-abstraction", "high", path, fact.line,
            format!("abstract base '{name}' declares {} methods but has {count} implementation(s) in the candidate tree",fact.methods),
            "Collapse the interface into its one implementation, or use a Protocol.",
            json!({"abstraction":name,"implementations":implementations})));
    }
    Ok(())
}

fn configs(
    index: &Index<'_>,
    changed: &BTreeSet<&str>,
    budget: &mut Budget,
    findings: &mut Vec<Value>,
    emit: bool,
) -> Result<(), String> {
    for (key, group) in &index.configs {
        if group.defaults.len() < 2 {
            continue;
        }
        let default_bytes = group.defaults.values().map(|s| s.len() + 2).sum::<usize>();
        for site in &group.sites {
            if !changed.contains(site.path) {
                continue;
            }
            budget
                .reserve(site.path.len() + key.len() + site.fact.default.len() + default_bytes)?;
            if !emit {
                continue;
            }
            let mut others = group
                .defaults
                .iter()
                .filter(|(key, _)| **key != site.fact.default_key)
                .map(|(_, display)| *display)
                .collect::<Vec<_>>();
            others.sort_unstable();
            findings.push(finding(
                "duplicate-config-default",
                "high",
                site.path,
                site.fact.line,
                format!(
                    "{key} defaults to {} here but to {} elsewhere in the candidate tree",
                    site.fact.default,
                    others.join(", ")
                ),
                "Read the key once into a shared constant and pass it down.",
                json!({"key":key,"default":site.fact.default,"conflicting":others}),
            ));
        }
    }
    Ok(())
}

fn evaluate_facts(facts: &[FileFacts], changed: &BTreeSet<&str>) -> Result<Value, String> {
    let index = index(facts);
    let mut findings = Vec::new();
    // Admit all findings and cross-module evidence before constructing any of it.
    for emit in [false, true] {
        let mut budget = Budget::new(changed)?;
        lifecycle(facts, changed, &mut budget, &mut findings, emit)?;
        locks(&index, changed, &mut budget, &mut findings, emit)?;
        abstractions(&index, changed, &mut budget, &mut findings, emit)?;
        configs(&index, changed, &mut budget, &mut findings, emit)?;
    }
    findings.sort_by(|a, b| {
        (
            a["path"].as_str(),
            a["line"].as_u64(),
            a["rule_id"].as_str(),
        )
            .cmp(&(
                b["path"].as_str(),
                b["line"].as_u64(),
                b["rule_id"].as_str(),
            ))
    });
    let output = json!({"findings":findings,"files":changed,"metrics":{
        "modules_indexed":facts.len(),"files_expected":facts.len(),"files_read":facts.len(),
        "files_skipped":0,"skipped_files":{}}});
    crate::candidate_cargo_audit::serialized_size(&output, MAX_OUTPUT)
        .map_err(|_| "structure-audit serialized output exceeds 32 MiB".to_owned())?;
    Ok(output)
}

/// Analyze the entire supplied inventory, including unchanged peers. Any peer
/// syntax/admission failure is explicit incomplete coverage, never an omitted file.
pub fn evaluate(payload: &Value) -> Result<Value, String> {
    let (records, changed) = inputs(payload)?;
    let facts = collect(&records, &changed)?;
    evaluate_facts(&facts, &changed)
}

#[cfg(test)]
#[path = "candidate_structure_policy_tests.rs"]
mod tests;
