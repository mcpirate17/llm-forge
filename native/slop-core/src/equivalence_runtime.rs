//! Native numerical-probe control flow. Object operations are delegated to a guest.
//! This module does not start an interpreter or mutate a source file.
use crate::{ablation, engine::Ablation};
use serde::Serialize;
use std::time::{Duration, Instant};

const CONTROL_REPEATS: usize = 3;
const CONTROL_MARGIN: f64 = 10.0;
const PLANS: [(&str, f64, bool); 8] = [
    ("x1e3", 1e3, false),
    ("x1e-3", 1e-3, false),
    ("x1e6", 1e6, false),
    ("neg", -1.0, false),
    ("zero", 0.0, false),
    ("params_x50", 50.0, true),
    ("params_x1e3", 1e3, true),
    ("params_zero", 0.0, true),
];

/// The host guest owns cloned Python objects and baseline memoization. It must
/// return None for an uncopyable call, matching exceptions, or an amplification
/// which does not change any argument. It must reset memos per function, and use
/// decorated and undecorated baselines as separate memo namespaces.
pub trait Guest {
    fn call_count(&self) -> usize;
    fn dropped(&self) -> &[String];
    fn compile(
        &mut self,
        source: &str,
        mutated: &str,
        qualname: &str,
        decorated: bool,
    ) -> Result<(), String>;
    /// A relative difference; controls compare baseline to itself, without memo.
    fn compare(
        &mut self,
        call: usize,
        plan: Option<(&str, f64, bool)>,
        control: bool,
        memo: bool,
    ) -> Result<Option<f64>, String>;
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultRow {
    pub qualname: String,
    pub rule: String,
    pub description: String,
    pub lineno: usize,
    pub verdict: &'static str,
    pub calls_seen: usize,
    pub usable_calls: usize,
    pub dropped_calls: usize,
    #[serde(serialize_with = "measurement")]
    pub max_diff_recorded: Option<f64>,
    #[serde(serialize_with = "measurement")]
    pub max_diff_amplified: Option<f64>,
    pub amplifier: Option<String>,
    #[serde(serialize_with = "measurement")]
    pub max_diff_control: Option<f64>,
    pub detail: String,
}
fn measurement<S: serde::Serializer>(value: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match value {
        Some(x) if x.is_nan() => s.serialize_str("nan"),
        Some(x) if *x == f64::INFINITY => s.serialize_str("inf"),
        Some(x) if *x == f64::NEG_INFINITY => s.serialize_str("-inf"),
        _ => value.serialize(s),
    }
}
struct Budget {
    deadline: Option<Instant>,
    cut: bool,
    seconds: Option<f64>,
}
impl Budget {
    fn new(seconds: Option<f64>) -> Result<Self, String> {
        let deadline = match seconds {
            Some(x) if !x.is_finite() => return Err("budget must be finite".into()),
            Some(x) if x > 0.0 => Some(
                Instant::now()
                    .checked_add(Duration::try_from_secs_f64(x).map_err(|e| e.to_string())?)
                    .ok_or("budget exceeds clock range")?,
            ),
            _ => None,
        };
        Ok(Self {
            deadline,
            cut: false,
            seconds,
        })
    }
    fn expired(&mut self) -> bool {
        self.cut |= self.deadline.is_some_and(|end| Instant::now() >= end);
        self.cut
    }
}
fn row(a: &Ablation, guest: &impl Guest) -> ResultRow {
    ResultRow {
        qualname: a.qualname.clone(),
        rule: a.rule.clone(),
        description: a.description.clone(),
        lineno: a.line,
        verdict: "NOT_EXERCISED",
        calls_seen: guest.call_count(),
        usable_calls: 0,
        dropped_calls: guest.dropped().len(),
        max_diff_recorded: None,
        max_diff_amplified: None,
        amplifier: None,
        max_diff_control: None,
        detail: String::new(),
    }
}
fn over_budget(row: &mut ResultRow, budget: &Budget) {
    row.verdict = "OVER_BUDGET";
    row.detail = format!("this function used its whole {}s sweep budget before reaching this construct, which was never ablated; re-run without a budget", budget.seconds.unwrap_or(0.0));
    row.max_diff_recorded = None;
    row.max_diff_amplified = None;
    row.amplifier = None;
}
fn recorded(
    guest: &mut impl Guest,
    budget: &mut Budget,
    noise: f64,
) -> Result<(f64, usize, bool), String> {
    let (mut worst, mut usable) = (0.0_f64, 0);
    for call in 0..guest.call_count() {
        if budget.expired() {
            break;
        }
        if let Some(diff) = guest.compare(call, None, false, true)? {
            if diff > worst {
                worst = diff;
            }
            usable += 1;
            if worst > noise {
                return Ok((worst, usable, true));
            }
        }
    }
    Ok((worst, usable, false))
}
fn amplified(
    guest: &mut impl Guest,
    mut budget: Option<&mut Budget>,
    control: bool,
    memo: bool,
) -> Result<(f64, Option<String>), String> {
    let (mut worst, mut which) = (0.0_f64, None);
    for plan in PLANS {
        if budget.as_deref_mut().is_some_and(Budget::expired) {
            break;
        }
        for call in 0..guest.call_count() {
            if budget.as_deref_mut().is_some_and(Budget::expired) {
                break;
            }
            if let Some(diff) = guest.compare(call, Some(plan), control, memo)? {
                if diff > worst {
                    worst = diff;
                    which = Some(plan.0.to_owned());
                }
            }
        }
    }
    Ok((worst, which))
}
fn adjudicate(guest: &mut impl Guest, row: &mut ResultRow, noise: f64) -> Result<(), String> {
    let mut effect = f64::INFINITY;
    let mut control = 0.0_f64;
    for _ in 0..CONTROL_REPEATS {
        effect = effect.min(amplified(guest, None, false, false)?.0);
    }
    for _ in 0..CONTROL_REPEATS {
        control = control.max(amplified(guest, None, true, false)?.0);
    }
    row.max_diff_amplified = Some(effect);
    row.max_diff_control = Some(control);
    if effect <= noise.max(control * CONTROL_MARGIN) {
        row.verdict = "NONDETERMINISTIC";
        row.detail = "the unmodified function disagrees with itself by as much as the ablation does, so the difference is not attributable to it".into();
    } else {
        row.verdict = "REACHABLE_BUT_UNTESTED";
    }
    Ok(())
}
fn one(
    source: &str,
    a: &Ablation,
    guest: &mut impl Guest,
    budget: &mut Budget,
    noise: f64,
) -> Result<ResultRow, String> {
    let mut out = row(a, guest);
    if out.calls_seen == 0 {
        if out.dropped_calls > 0 {
            out.verdict = "ARGUMENTS_UNCOPYABLE";
            let mut names = guest.dropped().to_vec();
            names.sort();
            names.dedup();
            out.detail = format!("{} recorded call(s) dropped: their arguments ({}) cannot be isolated from the run that produced them, so a replay would share mutable state with the baseline; nothing was measured", out.dropped_calls, names.iter().take(5).cloned().collect::<Vec<_>>().join(", "));
        }
        return Ok(out);
    }
    if budget.expired() {
        over_budget(&mut out, budget);
        return Ok(out);
    }
    let variant = match ablation::apply(source, a) {
        Ok(variant) => variant,
        Err(error) => {
            out.verdict = "UNCOMPILABLE";
            out.detail = error;
            return Ok(out);
        }
    };
    if let Err(error) = guest.compile(source, &variant, &a.qualname, a.rule == "drop_decorator") {
        out.verdict = "UNCOMPILABLE";
        out.detail = error;
        return Ok(out);
    }
    let (difference, usable, settled) = recorded(guest, budget, noise)?;
    out.max_diff_recorded = Some(difference);
    out.usable_calls = usable;
    if budget.cut {
        over_budget(&mut out, budget);
        return Ok(out);
    }
    if usable == 0 {
        out.verdict = "BASELINE_UNUSABLE";
        out.detail = "the recorded arguments never drove the unmodified function".into();
        return Ok(out);
    }
    if difference > noise {
        out.verdict = "LIVE";
        if settled {
            out.detail = format!("stopped after {usable} of {} recorded calls once the difference passed the {noise} noise floor; the construct is live either way, so max_diff_recorded is a lower bound", out.calls_seen);
        }
        return Ok(out);
    }
    let (difference, label) = amplified(guest, Some(&mut *budget), false, true)?;
    out.max_diff_amplified = Some(difference);
    out.amplifier = label;
    if budget.cut {
        over_budget(&mut out, budget);
    } else if difference > noise {
        adjudicate(guest, &mut out, noise)?;
    } else if out.max_diff_recorded.unwrap_or(0.0).max(difference) > 0.0 {
        out.verdict = "WITHIN_NUMERIC_NOISE";
    } else {
        out.verdict = "NO_DIFFERENCE_OBSERVED";
    }
    Ok(out)
}
fn pool_jitter(rows: &mut [ResultRow]) {
    let floor = rows
        .iter()
        .filter_map(|r| r.max_diff_control)
        .fold(0.0_f64, f64::max);
    if floor <= 0.0 {
        return;
    }
    for row in rows {
        if row.verdict == "REACHABLE_BUT_UNTESTED"
            && row.max_diff_amplified.unwrap_or(0.0) <= floor * CONTROL_MARGIN
        {
            row.verdict = "NONDETERMINISTIC";
            row.detail = format!("another ablation of this function measured the unmodified code disagreeing with itself by {floor:.3e}, which this difference does not clear");
        }
    }
}
/// Sweep one function, pooling the control jitter across all its constructs.
pub fn probe(
    source: &str,
    ablations: &[Ablation],
    guest: &mut impl Guest,
    noise: f64,
    budget_seconds: Option<f64>,
) -> Result<Vec<ResultRow>, String> {
    if !noise.is_finite() || noise < 0.0 {
        return Err("noise must be finite and nonnegative".into());
    }
    let mut budget = Budget::new(budget_seconds)?;
    let mut rows = Vec::with_capacity(ablations.len());
    for ablation in ablations {
        rows.push(one(source, ablation, guest, &mut budget, noise)?);
    }
    pool_jitter(&mut rows);
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        recorded: Option<f64>,
        effect: f64,
        control: f64,
        calls: usize,
        dropped: Vec<String>,
    }
    impl Guest for Fake {
        fn call_count(&self) -> usize {
            self.calls
        }
        fn dropped(&self) -> &[String] {
            &self.dropped
        }
        fn compile(&mut self, _: &str, _: &str, _: &str, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn compare(
            &mut self,
            _: usize,
            plan: Option<(&str, f64, bool)>,
            control: bool,
            _: bool,
        ) -> Result<Option<f64>, String> {
            Ok(if plan.is_some() {
                Some(if control { self.control } else { self.effect })
            } else {
                self.recorded
            })
        }
    }
    fn fixture() -> (String, Vec<Ablation>) {
        let source = "def f(x):\n    return x.clamp_min(0)\n".to_string();
        let chosen = ablation::select(None, &[]).unwrap();
        let rules = ablation::collect(&source, &chosen).unwrap();
        (source, rules)
    }
    #[test]
    fn sweeps_preserve_reachability_and_never_claim_equivalence() {
        let (source, rules) = fixture();
        assert!(!rules.is_empty());
        let mut guest = Fake {
            recorded: Some(0.0),
            effect: 0.0,
            control: 0.0,
            calls: 1,
            dropped: vec![],
        };
        assert!(probe(&source, &rules, &mut guest, 1e-6, None)
            .unwrap()
            .iter()
            .all(|r| r.verdict == "NO_DIFFERENCE_OBSERVED"));
        guest.effect = 0.1;
        assert!(probe(&source, &rules, &mut guest, 1e-6, None)
            .unwrap()
            .iter()
            .all(|r| r.verdict == "REACHABLE_BUT_UNTESTED"));
        guest.control = 0.02;
        assert!(probe(&source, &rules, &mut guest, 1e-6, None)
            .unwrap()
            .iter()
            .all(|r| r.verdict == "NONDETERMINISTIC"));
        guest.recorded = Some(0.5);
        assert!(probe(&source, &rules, &mut guest, 1e-6, None)
            .unwrap()
            .iter()
            .all(|r| r.verdict == "LIVE"));
    }
    #[test]
    fn unusable_and_dropped_calls_are_not_clean() {
        let (source, rules) = fixture();
        let mut guest = Fake {
            recorded: None,
            effect: 0.0,
            control: 0.0,
            calls: 1,
            dropped: vec![],
        };
        assert!(probe(&source, &rules, &mut guest, 1e-6, None)
            .unwrap()
            .iter()
            .all(|r| r.verdict == "BASELINE_UNUSABLE"));
        guest.calls = 0;
        guest.dropped.push("object".into());
        assert!(probe(&source, &rules, &mut guest, 1e-6, None)
            .unwrap()
            .iter()
            .all(|r| r.verdict == "ARGUMENTS_UNCOPYABLE"));
    }
    #[test]
    fn expired_budget_and_nonfinite_measurements_remain_explicit() {
        let (source, rules) = fixture();
        let mut guest = Fake {
            recorded: Some(0.0),
            effect: 0.0,
            control: 0.0,
            calls: 1,
            dropped: vec![],
        };
        let mut budget = Budget {
            deadline: Some(Instant::now()),
            cut: false,
            seconds: Some(0.1),
        };
        let result = one(&source, &rules[0], &mut guest, &mut budget, 1e-6).unwrap();
        assert_eq!(result.verdict, "OVER_BUDGET");
        assert!(result.max_diff_recorded.is_none());
        let mut result = row(&rules[0], &guest);
        result.max_diff_amplified = Some(f64::INFINITY);
        assert_eq!(
            serde_json::to_value(result).unwrap()["max_diff_amplified"],
            "inf"
        );
    }
    #[test]
    fn jitter_from_another_construct_sets_the_function_floor() {
        let (_, rules) = fixture();
        let guest = Fake {
            recorded: Some(0.0),
            effect: 0.0,
            control: 0.0,
            calls: 1,
            dropped: vec![],
        };
        let mut first = row(&rules[0], &guest);
        first.verdict = "REACHABLE_BUT_UNTESTED";
        first.max_diff_amplified = Some(0.1);
        let mut second = row(&rules[0], &guest);
        second.max_diff_control = Some(0.02);
        let mut rows = [first, second];
        pool_jitter(&mut rows);
        assert_eq!(rows[0].verdict, "NONDETERMINISTIC");
    }
}
