//! Native-engine reachability, jitter, and sweep contracts.
use crate::equivalence_probe_support::*;
use crate::support::module;
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyList, PyTuple};
use std::sync::{Arc, Mutex};

#[test]
fn whole_function_knockout_reaches_the_probe() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        assert!(rules(py, &ws, "scaled").contains(&"ablate_function_to_none".to_owned()))
    });
}
#[test]
fn the_probe_uses_the_native_engine_not_the_python_one() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        let native = module(py, "conductor._native")
            .getattr("slop_core")
            .unwrap()
            .call0()
            .unwrap();
        let pair = native.call_method0("rule_names").unwrap();
        let defaults = pair.get_item(0).unwrap();
        let names: Vec<String> = defaults.extract().unwrap();
        let python_rules = abl(py).getattr("RULES").unwrap();
        assert!(names.contains(&"ablate_function_to_none".to_owned()));
        assert!(names.len() > python_rules.len().unwrap());
        assert!(rules(py, &ws, "scaled").contains(&"ablate_function_to_none".to_owned()));
    });
}
#[test]
fn a_function_that_disagrees_with_itself_yields_no_blocking_finding() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        let values = verdicts(py, &ws, "timed");
        assert!(!values.is_empty());
        assert!(!values
            .iter()
            .any(|(_, v)| v == &verdict_constant(py, "REACHABLE_BUT_UNTESTED")));
        assert!(values
            .iter()
            .any(|(_, v)| v == &verdict_constant(py, "NONDETERMINISTIC")));
    });
}
#[test]
fn the_jitter_control_reports_rather_than_swallows() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        let unstable: Vec<_> = results(py, &ws, "timed")
            .into_iter()
            .filter(|r| result_verdict(r.bind(py)) == verdict_constant(py, "NONDETERMINISTIC"))
            .collect();
        assert!(!unstable.is_empty());
        for r in unstable {
            let control = r.bind(py).getattr("max_diff_control").unwrap();
            assert!(!control.is_none());
            assert!(control.extract::<f64>().unwrap() > 0.0);
        }
    });
}
#[test]
fn the_control_does_not_suppress_a_deterministic_finding() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        let blocking: Vec<_> = results(py, &ws, "Lane.forward")
            .into_iter()
            .filter(|r| {
                result_verdict(r.bind(py)) == verdict_constant(py, "REACHABLE_BUT_UNTESTED")
            })
            .collect();
        assert!(!blocking.is_empty());
        for r in blocking {
            assert_eq!(
                r.bind(py)
                    .getattr("max_diff_control")
                    .unwrap()
                    .extract::<f64>()
                    .unwrap(),
                0.0
            );
        }
    });
}
#[test]
fn the_jitter_floor_is_pooled_across_the_functions_ablations() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        let values = results(py, &ws, "timed");
        let floor = values
            .iter()
            .map(|r| {
                r.bind(py)
                    .getattr("max_diff_control")
                    .unwrap()
                    .extract::<Option<f64>>()
                    .unwrap()
                    .unwrap_or(0.0)
            })
            .fold(0.0, f64::max);
        assert!(floor > 0.0);
        assert!(!values
            .iter()
            .any(|r| result_verdict(r.bind(py)) == verdict_constant(py, "REACHABLE_BUT_UNTESTED")));
    });
}
#[test]
fn repeated_probes_of_unchanged_code_agree() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        let runs: Vec<_> = (0..3).map(|_| verdicts(py, &ws, "timed")).collect();
        assert_eq!(runs[0], runs[1]);
        assert_eq!(runs[1], runs[2]);
        for run in runs {
            assert!(!run
                .iter()
                .any(|(_, v)| v == &verdict_constant(py, "REACHABLE_BUT_UNTESTED")));
        }
    });
}
#[test]
fn a_decorator_ablation_reaches_the_probe() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        let values = verdicts(py, &ws, "decorated");
        assert!(values.iter().any(|(r, _)| r == "drop_decorator"));
        assert_eq!(
            values
                .into_iter()
                .find(|(r, _)| r == "drop_decorator")
                .unwrap()
                .1,
            verdict_constant(py, "LIVE")
        );
    });
}
#[test]
fn an_amplifier_that_cannot_touch_an_argument_is_not_replayed() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let seen = Arc::new(Mutex::new(0usize));
        let observed = seen.clone();
        let record = PyCFunction::new_closure(py, None, None, move |_args, _kwargs| {
            *observed.lock().unwrap() += 1;
            Ok::<f64, PyErr>(1.0)
        })
        .unwrap();
        let inner = PyTuple::new(
            py,
            [
                crate::support::path(py, std::path::Path::new("x")),
                {
                    let fields = PyDict::new(py);
                    fields.set_item("a", 1).unwrap();
                    fields.into_any()
                },
                "s".into_pyobject(py).unwrap().into_any(),
            ],
        )
        .unwrap();
        let inert_kwargs = PyDict::new(py);
        inert_kwargs.set_item("flag", true).unwrap();
        let inert = PyList::new(py, [(inner, inert_kwargs)]).unwrap();
        let (worst, which): (f64, Option<String>) = ep(py)
            .getattr("_sweep_amplified")
            .unwrap()
            .call1((&record, &record, &inert))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!((worst, which), (0.0, None));
        assert_eq!(*seen.lock().unwrap(), 0);
        let tensor = module(py, "torch")
            .getattr("ones")
            .unwrap()
            .call1((3,))
            .unwrap();
        let one =
            PyList::new(py, [(PyTuple::new(py, [tensor]).unwrap(), PyDict::new(py))]).unwrap();
        ep(py)
            .getattr("_sweep_amplified")
            .unwrap()
            .call1((&record, &record, &one))
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            2 * ep(py).getattr("AMPLIFIERS").unwrap().len().unwrap()
        );
        let torch = module(py, "torch");
        let tensor = torch.getattr("ones").unwrap().call1((2,)).unwrap();
        let doubled = tensor.call_method1("__mul__", (2,)).unwrap();
        let unchanged = ep(py).getattr("_unamplified").unwrap();
        assert!(!unchanged
            .call1((
                PyList::new(py, [tensor]).unwrap(),
                PyList::new(py, [doubled]).unwrap()
            ))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let first = PyList::empty(py);
        first.append(1).unwrap();
        first.append("a").unwrap();
        let second = PyList::empty(py);
        second.append(1).unwrap();
        second.append("a").unwrap();
        assert!(unchanged
            .call1((first, second,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let nested = PyList::new(py, [2, 3]).unwrap();
        let first = PyList::empty(py);
        first.append(1).unwrap();
        first.append(nested).unwrap();
        let nested = PyList::new(py, [2, 3]).unwrap();
        let second = PyList::empty(py);
        second.append(1).unwrap();
        second.append(nested).unwrap();
        assert!(unchanged
            .call1((first, second))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!unchanged
            .call1(((1, 2), PyList::new(py, [1, 2]).unwrap()))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!unchanged
            .call1((
                PyList::new(py, [1, 2]).unwrap(),
                PyList::new(py, [1]).unwrap()
            ))
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}
#[test]
fn a_settled_sweep_stops_early_and_says_the_number_is_a_lower_bound() {
    let ws = ProbeWorkspace::looping();
    Python::attach(|py| {
        let live: Vec<_> = results(py, &ws, "load_bearing")
            .into_iter()
            .filter(|r| result_verdict(r.bind(py)) == verdict_constant(py, "LIVE"))
            .collect();
        assert!(!live.is_empty());
        for r in live {
            let r = r.bind(py);
            let detail = r.getattr("detail").unwrap().extract::<String>().unwrap();
            assert!(detail.contains("stopped after 1 of 5 recorded calls"));
            assert_eq!(
                r.getattr("usable_calls")
                    .unwrap()
                    .extract::<usize>()
                    .unwrap(),
                1
            );
            assert!(
                r.getattr("max_diff_recorded")
                    .unwrap()
                    .extract::<Option<f64>>()
                    .unwrap()
                    .unwrap_or(0.0)
                    > 0.0
            );
        }
    });
}
#[test]
fn a_sweep_with_no_floor_returns_the_true_maximum() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let baseline = PyCFunction::new_closure(py, None, None, |args, _| {
            args.get_item(0).map(|v| v.unbind())
        })
        .unwrap();
        let variant = PyCFunction::new_closure(py, None, None, |args, _| {
            let x: f64 = args.get_item(0)?.extract()?;
            Ok::<f64, PyErr>(x * if x < 10.0 { 1.001 } else { 2.0 })
        })
        .unwrap();
        let calls = PyList::new(
            py,
            [
                (PyTuple::new(py, [1.0]).unwrap(), PyDict::new(py)),
                (PyTuple::new(py, [100.0]).unwrap(), PyDict::new(py)),
            ],
        )
        .unwrap();
        let sweep = ep(py).getattr("_sweep").unwrap();
        let (worst, usable, settled): (f64, usize, bool) = sweep
            .call1((&baseline, &variant, &calls))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!((usable, settled), (2, false));
        assert!((worst - 1.0).abs() <= 1e-6);
        let kwargs = PyDict::new(py);
        kwargs.set_item("settle_above", 1e-6).unwrap();
        let (first, usable, settled): (f64, usize, bool) = sweep
            .call((&baseline, &variant, &calls), Some(&kwargs))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!((usable, settled), (1, true));
        assert!((first - 0.001).abs() <= 1e-6);
        assert!(first < worst);
    });
}
