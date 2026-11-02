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
    #[serde(default)]
    pub cpus: Option<usize>,
    #[serde(default = "resources")]
    pub resources: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}
fn resources() -> Vec<String> {
    vec!["shared-environment".into()]
}
impl Default for Scheduling {
    fn default() -> Self {
        Self {
            cpus: None,
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
    let maximum = super::build_resources::maximum(&policy.env)?.max(2);
    for (name, rule) in &policy.schedule {
        ensure!(
            checks.contains(name.as_str()),
            "schedule names unknown check {name}"
        );
        ensure!(
            rule.cpus.is_none_or(|cpus| (1..=maximum).contains(&cpus)),
            "schedule {name}: cpus must fit the policy build CPU budget 1..={maximum}"
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

/// Explicit requests are never silently reduced to fit a smaller host.
pub(super) fn validate_capacity(policy: &Policy, cpu_budget: usize) -> Result<()> {
    for (name, rule) in &policy.schedule {
        ensure!(
            rule.cpus.is_none_or(|cpus| cpus <= cpu_budget),
            "schedule {name}: explicit CPU request exceeds admitted budget {cpu_budget}"
        );
    }
    Ok(())
}
fn reservation(policy: &Policy, index: usize, cpu_budget: usize) -> usize {
    policy
        .schedule
        .get(&policy.checks[index].name)
        .and_then(|rule| rule.cpus)
        .unwrap_or(cpu_budget)
}

fn batch(
    policy: &Policy,
    remaining: &BTreeSet<usize>,
    done: &[Option<StepRecord>],
    jobs: usize,
    cpu_budget: usize,
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
        let requested = reservation(policy, *index, cpu_budget);
        if result.len() >= jobs
            || cpus + requested > cpu_budget
            || rule.resources.iter().any(|r| resources.contains(r))
        {
            continue;
        }
        cpus += requested;
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
    pub cpu_budget: usize,
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
        cpu_budget,
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
        let indexes = batch(policy, &remaining, &done, jobs, cpu_budget);
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
                let check_env = super::build_resources::cap_environment(
                    env,
                    reservation(policy, *index, cpu_budget),
                )?;
                handles.push((
                    *index,
                    scope.spawn(move || {
                        if runnable {
                            run_step(
                                root,
                                step,
                                "check",
                                policy.setup.len() + *index,
                                &check_env,
                                dir,
                            )
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
            batch(&policy, &BTreeSet::from([0, 1]), &[None, None], 2, 2),
            [0, 1]
        );
        policy.schedule.get_mut("b").unwrap().resources = vec!["disk".into()];
        policy.schedule.get_mut("a").unwrap().resources = vec!["disk".into()];
        assert_eq!(
            batch(&policy, &BTreeSet::from([0, 1]), &[None, None], 2, 2),
            [0]
        );
        policy.schedule.get_mut("b").unwrap().depends_on = vec!["a".into()];
        policy.schedule.get_mut("a").unwrap().depends_on = vec!["b".into()];
        assert!(validate(&policy).is_err());
    }
    fn parallel_policy() -> Policy {
        Policy::parse(
            r#"base_ref="main"
[env]
CARGO_BUILD_JOBS="8"
RUST_TEST_THREADS="2"
[[check]]
name="a"
run="true"
timeout_s=1
[[check]]
name="b"
run="true"
timeout_s=1
[schedule.a]
cpus=4
resources=[]
[schedule.b]
cpus=4
resources=[]
"#,
        )
        .unwrap()
    }
    #[test]
    fn admitted_pool_and_check_count_are_independent_limits() {
        let policy = parallel_policy();
        let remaining = BTreeSet::from([0, 1]);
        assert_eq!(batch(&policy, &remaining, &[None, None], 2, 8), [0, 1]);
        assert_eq!(batch(&policy, &remaining, &[None, None], 1, 8), [0]);
        assert_eq!(batch(&policy, &remaining, &[None, None], 2, 4), [0]);
        assert!(validate_capacity(&policy, 4).is_ok());
        assert!(validate_capacity(&policy, 2).is_err());
    }
    #[test]
    fn implicit_checks_reserve_the_whole_admitted_pool_and_shared_resources_still_exclude() {
        let mut policy = parallel_policy();
        policy.schedule.clear();
        assert_eq!(reservation(&policy, 0, 8), 8);
        assert_eq!(
            batch(&policy, &BTreeSet::from([0, 1]), &[None, None], 2, 8),
            [0]
        );
        policy = parallel_policy();
        for rule in policy.schedule.values_mut() {
            rule.resources = vec!["shared-environment".into()];
        }
        assert_eq!(
            batch(&policy, &BTreeSet::from([0, 1]), &[None, None], 2, 8),
            [0]
        );
    }
    #[test]
    fn explicit_requests_cannot_exceed_policy_authority_or_be_silently_downsized() {
        let policy = parallel_policy();
        assert!(validate_capacity(&policy, 2).is_err());
        assert!(Policy::parse(
            r#"base_ref="main"
[env]
CARGO_BUILD_JOBS="8"
[[check]]
name="a"
run="true"
timeout_s=1
[schedule.a]
cpus=9
"#
        )
        .is_err());
    }
}
