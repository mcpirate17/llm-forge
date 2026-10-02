//! Opt-in check DAGs with conservative CPU and named-resource exclusion.
use super::{run_step, skipped, Policy, Source, StepRecord, Verdict};
use crate::land::selected;
use anyhow::{ensure, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Scheduling {
    #[serde(default = "cpus")]
    pub cpus: usize,
    #[serde(default = "resources")]
    pub resources: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}
fn cpus() -> usize {
    2
}
fn resources() -> Vec<String> {
    vec!["shared-environment".into()]
}
impl Default for Scheduling {
    fn default() -> Self {
        Self {
            cpus: cpus(),
            resources: resources(),
            depends_on: vec![],
        }
    }
}

pub fn validate(policy: &Policy) -> Result<()> {
    let checks: BTreeSet<_> = policy
        .checks
        .iter()
        .map(|step| step.name.as_str())
        .collect();
    for (name, rule) in &policy.schedule {
        ensure!(
            checks.contains(name.as_str()),
            "schedule names unknown check {name}"
        );
        ensure!(
            (1..=2).contains(&rule.cpus),
            "schedule {name}: cpus must be 1..2"
        );
        ensure!(
            rule.resources.iter().all(|r| !r.is_empty()),
            "schedule {name}: empty resource name"
        );
        for dependency in &rule.depends_on {
            ensure!(
                checks.contains(dependency.as_str()) && dependency != name,
                "schedule {name}: invalid dependency {dependency}"
            );
        }
    }
    let mut remaining = checks;
    let mut done = BTreeSet::new();
    while !remaining.is_empty() {
        let ready: Vec<_> = remaining
            .iter()
            .filter(|name| {
                policy.schedule.get(**name).is_none_or(|rule| {
                    rule.depends_on
                        .iter()
                        .all(|dep| done.contains(dep.as_str()))
                })
            })
            .copied()
            .collect();
        ensure!(
            !ready.is_empty(),
            "local-check schedule contains a dependency cycle"
        );
        for name in ready {
            remaining.remove(name);
            done.insert(name);
        }
    }
    Ok(())
}

fn batch(
    policy: &Policy,
    remaining: &BTreeSet<usize>,
    done: &[Option<StepRecord>],
    jobs: usize,
) -> Vec<usize> {
    let names: BTreeMap<_, _> = policy
        .checks
        .iter()
        .enumerate()
        .map(|(i, step)| (step.name.as_str(), i))
        .collect();
    let mut result = Vec::new();
    let mut cpus = 0;
    let mut resources = BTreeSet::new();
    for index in remaining {
        let rule = policy
            .schedule
            .get(&policy.checks[*index].name)
            .cloned()
            .unwrap_or_default();
        if rule
            .depends_on
            .iter()
            .any(|dep| done[names[dep.as_str()]].is_none())
        {
            continue;
        }
        if result.len() >= jobs
            || cpus + rule.cpus > 2
            || rule.resources.iter().any(|r| resources.contains(r))
        {
            continue;
        }
        cpus += rule.cpus;
        resources.extend(rule.resources);
        result.push(*index);
    }
    result
}

pub struct Execution<'a> {
    pub root: &'a Path,
    pub policy: &'a Policy,
    pub source: &'a Source,
    pub all: bool,
    pub setup_ok: bool,
    pub env: &'a [(String, String)],
    pub dir: &'a Path,
    pub jobs: usize,
}

pub fn execute(execution: Execution<'_>) -> Result<Vec<StepRecord>> {
    let Execution {
        root,
        policy,
        source,
        all,
        setup_ok,
        env,
        dir,
        jobs,
    } = execution;
    let mut done: Vec<Option<StepRecord>> = vec![None; policy.checks.len()];
    let names: BTreeMap<_, _> = policy
        .checks
        .iter()
        .enumerate()
        .map(|(i, step)| (step.name.as_str(), i))
        .collect();
    let mut remaining: BTreeSet<_> = (0..policy.checks.len()).collect();
    while !remaining.is_empty() {
        let indexes = batch(policy, &remaining, &done, jobs);
        ensure!(
            !indexes.is_empty(),
            "local-check scheduler has no runnable checks"
        );
        let outcomes = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for index in &indexes {
                let step = &policy.checks[*index];
                let deps_ok = policy.schedule.get(&step.name).is_none_or(|rule| {
                    rule.depends_on.iter().all(|dep| {
                        done[names[dep.as_str()]]
                            .as_ref()
                            .is_some_and(|r| r.verdict == Verdict::Passed)
                    })
                });
                let select = all || selected(step, &source.changed_paths);
                let runnable = setup_ok && deps_ok && select;
                handles.push((
                    *index,
                    scope.spawn(move || {
                        if runnable {
                            run_step(root, step, "check", policy.setup.len() + *index, env, dir)
                        } else {
                            Ok(skipped(step, "check"))
                        }
                    }),
                ));
            }
            handles
                .into_iter()
                .map(|(i, h)| {
                    Ok((
                        i,
                        h.join()
                            .map_err(|_| anyhow::anyhow!("check worker panicked"))??,
                    ))
                })
                .collect::<Result<Vec<_>>>()
        })?;
        for (index, record) in outcomes {
            remaining.remove(&index);
            done[index] = Some(record);
        }
    }
    Ok(done
        .into_iter()
        .map(|r| r.expect("completed scheduler entry"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dependencies_resources_and_cpu_budgets_control_batches() {
        let text = r#"base_ref="main"
[[check]]
name="a"
run="true"
timeout_s=1
[[check]]
name="b"
run="true"
timeout_s=1
[schedule.a]
cpus=1
resources=[]
[schedule.b]
cpus=1
resources=[]
"#;
        let mut policy = Policy::parse(text).unwrap();
        assert_eq!(
            batch(&policy, &BTreeSet::from([0, 1]), &[None, None], 2),
            [0, 1]
        );
        policy.schedule.get_mut("b").unwrap().resources = vec!["disk".into()];
        policy.schedule.get_mut("a").unwrap().resources = vec!["disk".into()];
        assert_eq!(
            batch(&policy, &BTreeSet::from([0, 1]), &[None, None], 2),
            [0]
        );
        policy.schedule.get_mut("b").unwrap().depends_on = vec!["a".into()];
        policy.schedule.get_mut("a").unwrap().depends_on = vec!["b".into()];
        assert!(validate(&policy).is_err());
    }
}
