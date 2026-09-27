//! Verdict and ablation generation contracts.
use crate::equivalence_probe_support::*;
use crate::support::{assert_error, module};
use pyo3::prelude::*;
use pyo3::types::PyDict;

#[test]
fn inert_guard_is_reported_as_no_difference() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        assert_eq!(
            verdict(py, &ws, "scaled", "drop_clamp_min"),
            verdict_constant(py, "NO_DIFFERENCE_OBSERVED")
        )
    });
}
#[test]
fn expensive_unproven_rules_are_off_unless_asked_for() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        assert!(!rules(py, &ws, "scaled").contains(&"ablate_function_to_passthrough".to_owned()));
        let kwargs = PyDict::new(py);
        kwargs.set_item("module_name", "fixture_mod").unwrap();
        kwargs
            .set_item("extra_rules", ("ablate_function_to_passthrough",))
            .unwrap();
        let opted = ep(py)
            .getattr("probe_function")
            .unwrap()
            .call(
                (ws.module_path(py), "scaled", ["test_fixture_mod.py"]),
                Some(&kwargs),
            )
            .unwrap();
        let mut seen = false;
        for r in opted.try_iter().unwrap() {
            let r = r.unwrap();
            if r.getattr("rule").unwrap().extract::<String>().unwrap()
                == "ablate_function_to_passthrough"
            {
                seen = true;
                assert_eq!(result_verdict(&r), verdict_constant(py, "LIVE"));
            }
        }
        assert!(seen);
        kwargs
            .set_item("extra_rules", ("no_such_rule", "also_missing"))
            .unwrap();
        let err = ep(py)
            .getattr("probe_function")
            .unwrap()
            .call(
                (ws.module_path(py), "scaled", ["test_fixture_mod.py"]),
                Some(&kwargs),
            )
            .unwrap_err();
        let msg = err.to_string();
        assert_error(
            py,
            err,
            module(py, "builtins").getattr("KeyError").unwrap().as_any(),
            "no_such_rule",
        );
        // The exception must enumerate every unknown rule, not just the first.
        assert!(msg.contains("also_missing"));
    });
}
#[test]
fn load_bearing_construct_is_reported_live() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        assert_eq!(
            verdict(py, &ws, "load_bearing", "drop_clamp_min"),
            verdict_constant(py, "LIVE")
        )
    });
}
#[test]
fn saturation_only_guard_is_untested_rather_than_dead() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        assert_eq!(
            verdict(py, &ws, "Lane.forward", "drop_trailing_arg"),
            verdict_constant(py, "REACHABLE_BUT_UNTESTED")
        )
    });
}
#[test]
fn float_round_off_is_not_reported_as_a_real_difference() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        assert_eq!(
            verdict(py, &ws, "renormalised", "drop_normalisation"),
            verdict_constant(py, "WITHIN_NUMERIC_NOISE")
        )
    });
}
#[test]
fn a_guard_driven_only_by_its_error_path_reads_live() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        assert_eq!(
            verdict(py, &ws, "validated", "drop_raise_guard"),
            verdict_constant(py, "LIVE")
        )
    });
}
#[test]
fn both_ends_failing_identically_is_inconclusive_not_agreement() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let mock = module(py, "unittest.mock").getattr("Mock").unwrap();
        let kwargs = PyDict::new(py);
        kwargs
            .set_item(
                "side_effect",
                module(py, "builtins")
                    .getattr("TypeError")
                    .unwrap()
                    .call1(("missing receiver",))
                    .unwrap(),
            )
            .unwrap();
        let boom = mock.call((), Some(&kwargs)).unwrap();
        assert!(ep(py)
            .getattr("_compare_one")
            .unwrap()
            .call1((&boom, &boom, (), PyDict::new(py)))
            .unwrap()
            .is_none());
    });
}
#[test]
fn a_call_that_exits_the_process_is_measured_not_propagated() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let mock = module(py, "unittest.mock").getattr("Mock").unwrap();
        let kwargs = PyDict::new(py);
        kwargs
            .set_item(
                "side_effect",
                module(py, "builtins")
                    .getattr("SystemExit")
                    .unwrap()
                    .call1((2,))
                    .unwrap(),
            )
            .unwrap();
        let exits = mock.call((), Some(&kwargs)).unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("return_value", 1).unwrap();
        let returns = mock.call((), Some(&kwargs)).unwrap();
        let compare = ep(py).getattr("_compare_one").unwrap();
        assert!(compare
            .call1((&exits, &exits, (), PyDict::new(py)))
            .unwrap()
            .is_none());
        assert!(compare
            .call1((&returns, &exits, (), PyDict::new(py)))
            .unwrap()
            .extract::<f64>()
            .unwrap()
            .is_infinite());
        assert!(compare
            .call1((&exits, &returns, (), PyDict::new(py)))
            .unwrap()
            .extract::<f64>()
            .unwrap()
            .is_infinite());
    });
}
#[test]
fn methods_record_their_receiver() {
    let ws = ProbeWorkspace::methods();
    Python::attach(|py| {
        let results = probe(py, &ws, "Lane.forward");
        assert!(results.len().unwrap() > 0);
        for r in results.try_iter().unwrap() {
            let r = r.unwrap();
            assert!(
                r.getattr("usable_calls")
                    .unwrap()
                    .extract::<usize>()
                    .unwrap()
                    > 0
            );
            assert_ne!(
                result_verdict(&r),
                verdict_constant(py, "BASELINE_UNUSABLE")
            );
        }
    });
}
#[test]
fn trailing_argument_rule_needs_a_defaulted_callee() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let tree = fixture_ast(py);
        let lane = named_child(&tree.getattr("body").unwrap(), "Lane");
        let forward = named_child(&lane.getattr("body").unwrap(), "forward");
        assert!(
            generated_rules(py, &forward, Some(&tree)).contains(&"drop_trailing_arg".to_owned())
        );
        assert!(!generated_rules(py, &forward, None).contains(&"drop_trailing_arg".to_owned()));
        let helper = named_child(&tree.getattr("body").unwrap(), "masked_softmax");
        assert!(generated_rules(py, &helper, Some(&tree)).contains(&"drop_where".to_owned()));
    });
}
#[test]
fn unary_call_rule_needs_a_one_argument_callee() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let tree = unary_ast(py);
        let caller = named_child(&tree.getattr("body").unwrap(), "caller");
        let generated = abl(py)
            .getattr("generate_ablations")
            .unwrap()
            .call1((&caller, &tree))
            .unwrap();
        let descriptions: Vec<String> = generated
            .try_iter()
            .unwrap()
            .filter_map(|r| {
                let r = r.unwrap();
                (r.getattr("rule").unwrap().extract::<String>().unwrap() == "drop_unary_call")
                    .then(|| r.getattr("description").unwrap().extract().unwrap())
            })
            .collect();
        assert!(descriptions.iter().any(|d| d.contains("unary")));
        assert!(!descriptions.iter().any(|d| d.contains("binary")));
    });
}
#[test]
fn a_module_probe_shares_one_driver_run_across_its_functions() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        let targets = public_targets(py, &ws);
        assert!(targets.len() > 1);
        let (_patch, runs) = count_pytest_runs(py);
        ep(py)
            .getattr("probe_module")
            .unwrap()
            .call1((ws.module_path(py), ["test_fixture_mod.py"]))
            .unwrap();
        assert_eq!(*runs.lock().unwrap(), 1);
    });
}
#[test]
fn recorders_are_removed_after_a_shared_run() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        let fixture = import_fixture(py);
        let targets = public_targets(py, &ws);
        ep(py)
            .getattr("_record_many")
            .unwrap()
            .call1((&fixture, &targets, ["test_fixture_mod.py"]))
            .unwrap();
        for name in targets {
            let mut owner = fixture.as_any().clone();
            let parts: Vec<_> = name.split('.').collect();
            for part in &parts[..parts.len() - 1] {
                owner = owner.getattr(*part).unwrap();
            }
            let bound = owner.getattr(*parts.last().unwrap()).unwrap();
            let marker = bound
                .getattr("__equivalence_recorder__")
                .ok()
                .and_then(|m| m.extract::<bool>().ok())
                .unwrap_or(false);
            assert!(!marker, "{name} retained recorder");
        }
    });
}
#[test]
fn batching_bounds_recorder_memory_without_losing_a_target() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        let fixture = import_fixture(py);
        let targets = public_targets(py, &ws);
        assert!(targets.len() >= 2);
        let (_patch, runs) = count_pytest_runs(py);
        let kwargs = PyDict::new(py);
        kwargs.set_item("batch", 1).unwrap();
        let recorded = ep(py)
            .getattr("_record_many")
            .unwrap()
            .call((&fixture, &targets, ["test_fixture_mod.py"]), Some(&kwargs))
            .unwrap();
        assert_eq!(*runs.lock().unwrap(), targets.len());
        let got: Vec<String> = recorded.cast::<PyDict>().unwrap().keys().extract().unwrap();
        let mut got = got;
        let mut targets = targets;
        got.sort();
        targets.sort();
        assert_eq!(got, targets);
    });
}
