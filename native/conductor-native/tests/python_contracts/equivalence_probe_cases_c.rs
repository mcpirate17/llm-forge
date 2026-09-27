//! Replay isolation, dropped recordings, and stub override contracts.
use crate::equivalence_probe_support::*;
use crate::support::{module, text};
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
#[test]
fn replay_stub_overrides_swaps_attributes_and_captured_defaults() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let subjects = clone_subject_module(py);
        let target = create_stub_target(py, "fixture_probe_stub_target");
        let real = subjects.getattr("_real_embed").unwrap();
        let original_path = subjects.getattr("real_path").unwrap();
        let query = subjects.getattr("query").unwrap();
        query
            .setattr("__module__", "fixture_probe_stub_target")
            .unwrap();
        target.setattr("embed_text", &real).unwrap();
        target.setattr("INDEX_PATH", &original_path).unwrap();
        target.setattr("query", &query).unwrap();
        let original_defaults = query.getattr("__defaults__").unwrap().unbind();
        let stub = subjects.getattr("_stub_embed").unwrap();
        let spec = PyDict::new(py);
        spec.set_item("embed_text", &stub).unwrap();
        spec.set_item("INDEX_PATH", prs(py).getattr("TMP_PATH").unwrap())
            .unwrap();
        let mut override_guard = ReplayStubOverride::new(py, &target, &spec);
        let tmp = module(py, "tempfile")
            .getattr("gettempdir")
            .unwrap()
            .call0()
            .unwrap()
            .extract::<String>()
            .unwrap();
        assert!(target.getattr("embed_text").unwrap().is(&stub));
        assert!(text(&target.getattr("INDEX_PATH").unwrap()).starts_with(&tmp));
        let defaults = query.getattr("__defaults__").unwrap();
        assert!(defaults.get_item(0).unwrap().is(&stub));
        assert!(text(&defaults.get_item(1).unwrap()).starts_with(&tmp));
        override_guard.close(py).unwrap();
        assert!(target.getattr("embed_text").unwrap().is(&real));
        assert!(target.getattr("INDEX_PATH").unwrap().is(&original_path));
        assert!(query
            .getattr("__defaults__")
            .unwrap()
            .eq(original_defaults.bind(py))
            .unwrap());
    });
}
#[test]
fn replay_stub_creates_then_deletes_attrs_that_did_not_exist() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let target = create_stub_target(py, "fixture_probe_stub_missing");
        let marker = module(py, "builtins")
            .getattr("object")
            .unwrap()
            .call0()
            .unwrap();
        let spec = PyDict::new(py);
        spec.set_item("ghost", &marker).unwrap();
        let mut override_guard = ReplayStubOverride::new(py, &target, &spec);
        assert!(target.getattr("ghost").unwrap().is(&marker));
        override_guard.close(py).unwrap();
        assert!(target.getattr("ghost").is_err());
    });
}
#[test]
fn stub_embeddings_are_deterministic_and_satisfy_the_meta_contract() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let stub = prs(py).getattr("_stub_embed_text").unwrap();
        let first = stub.call1(("abc",)).unwrap();
        assert!(first.eq(stub.call1(("abc",)).unwrap()).unwrap());
        let dimension = prs(py)
            .getattr("STUB_DIMENSION")
            .unwrap()
            .extract::<usize>()
            .unwrap();
        assert_eq!(first.len().unwrap(), dimension);
        let kwargs = PyDict::new(py);
        kwargs.set_item("purpose", "query").unwrap();
        let query = stub.call(("abc",), Some(&kwargs)).unwrap();
        assert!(!query.eq(&first).unwrap());
        assert!(query
            .eq(stub.call(("abc",), Some(&kwargs)).unwrap())
            .unwrap());
        let batch = prs(py)
            .getattr("_stub_embed_batch")
            .unwrap()
            .call1((["x", "y"],))
            .unwrap();
        let vectors = batch.getattr("vectors").unwrap();
        assert_eq!(vectors.len().unwrap(), 2);
        for vector in vectors.try_iter().unwrap() {
            assert_eq!(vector.unwrap().len().unwrap(), dimension);
        }
        let payload = PyDict::new(py);
        payload
            .set_item("embedding", batch.getattr("metadata").unwrap())
            .unwrap();
        let actual = module(py, "conductor.kb_retrieve")
            .getattr("assert_embedding_meta")
            .unwrap()
            .call1((&payload,))
            .unwrap();
        assert!(actual.eq(batch.getattr("metadata").unwrap()).unwrap());
    });
}
#[test]
fn unlisted_modules_get_no_overrides() {
    let _case = crate::support::Case::new();
    Python::attach(|py| {
        let target = create_stub_target(py, "fixture_probe_stub_unlisted");
        let stub = prs(py).getattr("_stub_embed_text").unwrap();
        target.setattr("embed_text", &stub).unwrap();
        target
            .setattr("INDEX_PATH", "/tmp/real-index.jsonl")
            .unwrap();
        let before_embed = target.getattr("embed_text").unwrap().unbind();
        let before_path = target.getattr("INDEX_PATH").unwrap().unbind();
        let context = prs(py)
            .getattr("replay_stub_overrides")
            .unwrap()
            .call1((&target,))
            .unwrap();
        context.call_method0("__enter__").unwrap();
        assert!(target
            .getattr("embed_text")
            .unwrap()
            .is(before_embed.bind(py)));
        assert!(target
            .getattr("INDEX_PATH")
            .unwrap()
            .is(before_path.bind(py)));
        context
            .call_method1("__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
        assert!(target
            .getattr("embed_text")
            .unwrap()
            .is(before_embed.bind(py)));
        assert!(target
            .getattr("INDEX_PATH")
            .unwrap()
            .is(before_path.bind(py)));
    });
}
