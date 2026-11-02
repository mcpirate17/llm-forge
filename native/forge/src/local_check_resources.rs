//! Advisory build capacity observed through the existing resource admission path.
//! These observations neither reserve resources nor enforce OS limits. No GPU is
//! requested or probed. Keep the two-thread Rust test baseline in the CPU budget.

use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};
use serde::Serialize;

use crate::task_resources::{ResourceRequest, ResourceSnapshot};

const DEFAULT_MAXIMUM: usize = 2;
const MAXIMUM_LIMIT: usize = 64;
const TEST_THREADS: usize = 2;
const MIB_PER_CPU: u64 = 4096;

#[derive(Debug, Clone, Serialize)]
pub(super) struct BuildResources {
    pub maximum: usize,
    pub workers: usize,
    pub cpu_budget: usize,
    pub memory_mib: u64,
    pub snapshot: Option<ResourceSnapshot>,
}

pub(super) fn maximum(env: &BTreeMap<String, String>) -> Result<usize> {
    setting(env, "CARGO_BUILD_JOBS", DEFAULT_MAXIMUM)
}
pub(super) fn test_threads(env: &BTreeMap<String, String>) -> Result<usize> {
    setting(env, "RUST_TEST_THREADS", TEST_THREADS)
}
fn setting(env: &BTreeMap<String, String>, key: &str, default: usize) -> Result<usize> {
    let Some(value) = env.get(key) else {
        return Ok(default);
    };
    ensure!(
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
        "{key} must be a decimal integer in 1..=64, got {value:?}"
    );
    let count = value
        .parse::<usize>()
        .with_context(|| format!("{key} must fit in 1..=64, got {value:?}"))?;
    ensure!(
        (1..=MAXIMUM_LIMIT).contains(&count),
        "{key} must be in 1..=64, got {count}"
    );
    Ok(count)
}

pub(super) fn admit(maximum: usize) -> Result<BuildResources> {
    validate_maximum(maximum)?;
    if maximum <= TEST_THREADS {
        return legacy(maximum);
    }
    let snapshot = ResourceRequest {
        cpus: Some(TEST_THREADS as f64),
        memory_mib: Some(TEST_THREADS as u64 * MIB_PER_CPU),
        ..ResourceRequest::default()
    }
    .admit()
    .context("admitting local-check build resource baseline")?;
    select(maximum, snapshot)
}

/// Retain legacy policy setup admission rather than introducing a host probe.
pub(super) fn legacy(maximum: usize) -> Result<BuildResources> {
    validate_maximum(maximum)?;
    ensure!(
        maximum <= TEST_THREADS,
        "legacy build maximum must be 1 or 2"
    );
    Ok(BuildResources {
        maximum,
        workers: maximum,
        cpu_budget: TEST_THREADS,
        memory_mib: TEST_THREADS as u64 * MIB_PER_CPU,
        snapshot: None,
    })
}

/// Bound an already selected environment to this check's scheduler allocation.
pub(super) fn cap_environment(
    env: &[(String, String)],
    cpus: usize,
) -> Result<Vec<(String, String)>> {
    ensure!(cpus > 0, "check CPU allocation must be positive");
    let mut selected = BTreeMap::new();
    for (key, value) in env {
        if matches!(
            key.as_str(),
            "CARGO_BUILD_JOBS" | "RUST_TEST_THREADS" | "FORGE_CHECK_BUILD_MEMORY_MIB"
        ) {
            ensure!(
                selected.insert(key.clone(), value.clone()).is_none(),
                "duplicate build environment entry: {key}"
            );
        }
    }
    let workers = maximum(&selected)?.min(cpus);
    let threads = test_threads(&selected)?.min(cpus);
    let memory_mib = workers.max(TEST_THREADS) as u64 * MIB_PER_CPU;
    let mut capped = env.to_vec();
    for (key, value) in [
        ("CARGO_BUILD_JOBS", workers.to_string()),
        ("RUST_TEST_THREADS", threads.to_string()),
        ("FORGE_CHECK_BUILD_MEMORY_MIB", memory_mib.to_string()),
    ] {
        if let Some((_, current)) = capped.iter_mut().find(|(name, _)| name == key) {
            *current = value;
        } else {
            capped.push((key.to_owned(), value));
        }
    }
    Ok(capped)
}

fn validate_maximum(maximum: usize) -> Result<()> {
    ensure!(
        (1..=MAXIMUM_LIMIT).contains(&maximum),
        "CARGO_BUILD_JOBS must be in 1..=64, got {maximum}"
    );
    Ok(())
}

fn select(maximum: usize, snapshot: ResourceSnapshot) -> Result<BuildResources> {
    validate_maximum(maximum)?;
    ensure!(
        snapshot.effective_cpus.is_finite() && snapshot.effective_cpus >= TEST_THREADS as f64,
        "local checks require at least 2 effective CPUs, observed {}",
        snapshot.effective_cpus
    );
    ensure!(
        snapshot.memory_available_mib >= TEST_THREADS as u64 * MIB_PER_CPU,
        "local checks require at least 8192 MiB available RAM, observed {} MiB",
        snapshot.memory_available_mib
    );
    let cpu_workers = snapshot.effective_cpus.floor().min(MAXIMUM_LIMIT as f64) as usize;
    let memory_workers =
        (snapshot.memory_available_mib / MIB_PER_CPU).min(MAXIMUM_LIMIT as u64) as usize;
    let workers = maximum.min(cpu_workers).min(memory_workers);
    let cpu_budget = workers.max(TEST_THREADS);
    Ok(BuildResources {
        maximum,
        workers,
        cpu_budget,
        memory_mib: cpu_budget as u64 * MIB_PER_CPU,
        snapshot: Some(snapshot),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(cpus: f64, available_mib: u64) -> ResourceSnapshot {
        ResourceSnapshot {
            captured_at: 123,
            allowed_cpus: 16,
            effective_cpus: cpus,
            load_1m: 0.0,
            load_5m: 0.0,
            load_15m: 0.0,
            memory_total_mib: 131072,
            memory_available_mib: available_mib,
            cgroup_v2_path: None,
            cgroup_memory_limit_mib: None,
            cgroup_memory_available_mib: None,
            gpu: None,
        }
    }

    #[test]
    fn configured_maximum_is_bounded_decimal_with_a_two_worker_default() {
        assert_eq!(maximum(&BTreeMap::new()).unwrap(), 2);
        for (value, expected) in [("1", 1), ("8", 8), ("64", 64), ("02", 2)] {
            let env = BTreeMap::from([("CARGO_BUILD_JOBS".into(), value.into())]);
            assert_eq!(maximum(&env).unwrap(), expected);
        }
        for value in [
            "",
            "0",
            "-1",
            "+2",
            "65",
            "1.0",
            " 2",
            "2 ",
            "unlimited",
            "２",
            "99999999999999999999999999999999999999999999999999999",
        ] {
            let env = BTreeMap::from([("CARGO_BUILD_JOBS".into(), value.into())]);
            assert!(maximum(&env).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn capable_host_uses_requested_workers_and_preserves_observation() {
        let resources = select(8, snapshot(16.0, 65536)).unwrap();
        assert_eq!(resources.maximum, 8);
        assert_eq!(resources.workers, 8);
        assert_eq!(resources.cpu_budget, 8);
        assert_eq!(resources.memory_mib, 32768);
        let snapshot = resources.snapshot.unwrap();
        assert_eq!(snapshot.captured_at, 123);
        assert_eq!(snapshot.effective_cpus, 16.0);
        assert!(snapshot.gpu.is_none());
    }

    #[test]
    fn fractional_cpu_quota_rounds_down() {
        let resources = select(8, snapshot(2.5, 65536)).unwrap();
        assert_eq!(resources.workers, 2);
        assert_eq!(resources.cpu_budget, 2);
        assert_eq!(resources.memory_mib, 8192);
    }

    #[test]
    fn available_memory_includes_cgroup_headroom() {
        let mut observation = snapshot(16.0, 12287);
        observation.cgroup_memory_limit_mib = Some(16384);
        observation.cgroup_memory_available_mib = Some(12287);
        let resources = select(8, observation).unwrap();
        assert_eq!(resources.workers, 2);
        assert_eq!(resources.memory_mib, 8192);
        assert_eq!(
            resources.snapshot.unwrap().cgroup_memory_available_mib,
            Some(12287)
        );
        assert_eq!(select(8, snapshot(16.0, 8192)).unwrap().workers, 2);
    }

    #[test]
    fn single_worker_keeps_two_thread_test_budget() {
        let resources = select(1, snapshot(8.0, 65536)).unwrap();
        assert_eq!(resources.maximum, 1);
        assert_eq!(resources.workers, 1);
        assert_eq!(resources.cpu_budget, 2);
        assert_eq!(resources.memory_mib, 8192);
    }

    #[test]
    fn legacy_plans_preserve_fixed_admission_without_a_snapshot() {
        for maximum in [1, 2] {
            let resources = legacy(maximum).unwrap();
            assert_eq!(resources.maximum, maximum);
            assert_eq!(resources.workers, maximum);
            assert_eq!(resources.cpu_budget, 2);
            assert_eq!(resources.memory_mib, 8192);
            assert!(resources.snapshot.is_none());
        }
        for maximum in [0, 3, 65] {
            assert!(legacy(maximum).is_err());
        }
    }

    #[test]
    fn environment_cap_bounds_build_and_test_workers_to_each_cpu_allocation() {
        let env = vec![
            ("CARGO_BUILD_JOBS".into(), "8".into()),
            ("RUST_TEST_THREADS".into(), "2".into()),
            ("FORGE_CHECK_BUILD_MEMORY_MIB".into(), "32768".into()),
            ("OTHER".into(), "unchanged".into()),
        ];
        for (cpus, expected_workers, expected_threads, expected_memory) in
            [(3, "3", "2", "12288"), (1, "1", "1", "8192")]
        {
            let capped: BTreeMap<_, _> = cap_environment(&env, cpus).unwrap().into_iter().collect();
            assert_eq!(capped["CARGO_BUILD_JOBS"], expected_workers);
            assert_eq!(capped["FORGE_CHECK_BUILD_MEMORY_MIB"], expected_memory);
            assert_eq!(capped["RUST_TEST_THREADS"], expected_threads);
            assert_eq!(capped["OTHER"], "unchanged");
        }
        assert_eq!(cap_environment(&env, 16).unwrap(), env);
        let defaults: BTreeMap<_, _> = cap_environment(&[], 8).unwrap().into_iter().collect();
        assert_eq!(defaults["CARGO_BUILD_JOBS"], "2");
        assert_eq!(defaults["RUST_TEST_THREADS"], "2");
        let single: BTreeMap<_, _> = cap_environment(&[], 1).unwrap().into_iter().collect();
        assert_eq!(single["CARGO_BUILD_JOBS"], "1");
        assert_eq!(single["RUST_TEST_THREADS"], "1");
        assert_eq!(defaults["FORGE_CHECK_BUILD_MEMORY_MIB"], "8192");
    }

    #[test]
    fn environment_cap_rejects_invalid_worker_values_and_duplicate_owned_keys() {
        assert!(cap_environment(&[], 0).is_err());
        for key in ["CARGO_BUILD_JOBS", "RUST_TEST_THREADS"] {
            for value in ["0", "65", "+2", "unlimited", "", "-1"] {
                let env = vec![(key.into(), value.into())];
                assert!(cap_environment(&env, 8).is_err());
            }
        }
        for key in [
            "CARGO_BUILD_JOBS",
            "RUST_TEST_THREADS",
            "FORGE_CHECK_BUILD_MEMORY_MIB",
        ] {
            let env = vec![(key.into(), "2".into()), (key.into(), "2".into())];
            assert!(cap_environment(&env, 8).is_err());
        }
    }

    #[test]
    fn selector_rejects_invalid_limits_or_inadmissible_observations() {
        for limit in [0, 65, usize::MAX] {
            assert!(select(limit, snapshot(16.0, 65536)).is_err());
        }
        for cpus in [0.0, 1.99, -1.0, f64::NAN, f64::INFINITY] {
            assert!(select(8, snapshot(cpus, 65536)).is_err());
        }
        assert!(select(8, snapshot(16.0, 8191)).is_err());
    }
}
