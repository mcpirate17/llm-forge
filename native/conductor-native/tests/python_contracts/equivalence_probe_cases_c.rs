//! Replay isolation and dropped-recording contracts.
use crate::equivalence_probe_support::*;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

fn uncopyable<'py>(
    _py: Python<'py>,
    subjects: &Bound<'py, pyo3::types::PyModule>,
    value: &str,
) -> Bound<'py, pyo3::types::PyAny> {
    subjects
        .getattr("_Uncopyable")
        .unwrap()
        .call1(([value],))
        .unwrap()
}
#[test]
fn an_uncopyable_argument_is_refused_rather_than_shared() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let subjects = clone_subject_module(py);
        let payload = uncopyable(py, &subjects, "abcd");
        let clone = ep(py).getattr("_clone").unwrap();
        let error_class = ep(py).getattr("UncopyableValue").unwrap();
        let err = clone.call1((&payload,)).unwrap_err();
        assert!(err.matches(py, &error_class).unwrap());
        assert_eq!(
            err.value(py)
                .getattr("type_name")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "_Uncopyable"
        );
        let nested = PyTuple::new(
            py,
            [PyList::new(py, [&payload]).unwrap().into_any(), {
                let dict = PyDict::new(py);
                dict.set_item("k", &payload).unwrap();
                dict.into_any()
            }],
        )
        .unwrap();
        let err = clone.call1((&nested,)).unwrap_err();
        assert!(err.matches(py, &error_class).unwrap());
        let items = payload.getattr("items").unwrap();
        assert_eq!(items.extract::<Vec<String>>().unwrap(), vec!["abcd"]);
    });
}
#[test]
fn the_baseline_never_differs_from_itself_on_an_uncopyable_argument() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let subjects = clone_subject_module(py);
        let payload = uncopyable(py, &subjects, "abcd");
        let consume = subjects.getattr("_consume").unwrap();
        let args = PyTuple::new(py, [&payload]).unwrap();
        let result = ep(py)
            .getattr("_compare_one")
            .unwrap()
            .call1((&consume, &consume, &args, PyDict::new(py)))
            .unwrap();
        assert!(result.is_none());
        assert_eq!(
            payload
                .getattr("items")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec!["abcd"]
        );
    });
}
#[test]
fn a_dropped_recording_is_counted_rather_than_swallowed() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let subjects = clone_subject_module(py);
        let consume = subjects.getattr("_consume").unwrap();
        let pair = ep(py)
            .getattr("_make_recorder")
            .unwrap()
            .call1((&consume,))
            .unwrap();
        let recorder = pair.get_item(0).unwrap();
        let recording = pair.get_item(1).unwrap();
        assert_eq!(
            recorder
                .call1((uncopyable(py, &subjects, "ab"),))
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            2
        );
        assert_eq!(
            recorder
                .call1((uncopyable(py, &subjects, "xyz"),))
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            3
        );
        assert_eq!(recording.getattr("calls").unwrap().len().unwrap(), 0);
        assert_eq!(
            recording
                .getattr("dropped")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec!["_Uncopyable", "_Uncopyable"]
        );
        let kept = ep(py)
            .getattr("_make_recorder")
            .unwrap()
            .call1((&consume,))
            .unwrap();
        assert_eq!(
            kept.get_item(0)
                .unwrap()
                .call1((uncopyable(py, &subjects, "ab"),))
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            2
        );
        assert_eq!(
            kept.get_item(1)
                .unwrap()
                .getattr("dropped")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec!["_Uncopyable"]
        );
    });
}
#[test]
fn the_drop_limit_counts_refused_calls_too() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let subjects = clone_subject_module(py);
        let kwargs = PyDict::new(py);
        kwargs.set_item("limit", 3).unwrap();
        let pair = ep(py)
            .getattr("_make_recorder")
            .unwrap()
            .call((subjects.getattr("_consume").unwrap(),), Some(&kwargs))
            .unwrap();
        for _ in 0..10 {
            pair.get_item(0)
                .unwrap()
                .call1((uncopyable(py, &subjects, "ab"),))
                .unwrap();
        }
        assert_eq!(
            pair.get_item(1)
                .unwrap()
                .getattr("dropped")
                .unwrap()
                .len()
                .unwrap(),
            3
        );
    });
}
#[test]
fn uncopyable_arguments_report_unusable_evidence_not_a_clean_sweep() {
    let ws = ProbeWorkspace::new();
    Python::attach(|py| {
        let target = public_targets(py, &ws).into_iter().next().unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("calls", PyList::empty(py)).unwrap();
        kwargs.set_item("dropped", ("_Uncopyable", "Lock")).unwrap();
        let function = ep(py).getattr("probe_function").unwrap();
        let found = function
            .call(
                (ws.module_path(py), &target, ["test_fixture_mod.py"]),
                Some(&kwargs),
            )
            .unwrap();
        assert!(found.len().unwrap() > 0);
        for r in found.try_iter().unwrap() {
            let r = r.unwrap();
            assert_eq!(
                result_verdict(&r),
                verdict_constant(py, "ARGUMENTS_UNCOPYABLE")
            );
            assert_eq!(
                r.getattr("dropped_calls")
                    .unwrap()
                    .extract::<usize>()
                    .unwrap(),
                2
            );
            let detail = r.getattr("detail").unwrap().extract::<String>().unwrap();
            assert!(detail.contains("_Uncopyable") && detail.contains("Lock"));
        }
        kwargs.del_item("dropped").unwrap();
        let untouched = function
            .call(
                (ws.module_path(py), &target, ["test_fixture_mod.py"]),
                Some(&kwargs),
            )
            .unwrap();
        let mut count = 0;
        for r in untouched.try_iter().unwrap() {
            let r = r.unwrap();
            count += 1;
            assert_eq!(result_verdict(&r), verdict_constant(py, "NOT_EXERCISED"));
            assert_eq!(
                r.getattr("dropped_calls")
                    .unwrap()
                    .extract::<usize>()
                    .unwrap(),
                0
            );
        }
        assert!(count > 0);
    });
}
