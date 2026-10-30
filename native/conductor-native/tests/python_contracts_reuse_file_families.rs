#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for exact file-family scoring and complete-link groups.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyFrozenSet, PyList, PySet, PyTuple};
use serde_json::{json, Value};
use std::ffi::CString;
use support::{module, Case};

fn families(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.reuse.file_families")
}

fn frozen<'py>(
    py: Python<'py>,
    values: impl IntoIterator<Item = String>,
) -> Bound<'py, PyFrozenSet> {
    PyFrozenSet::new(py, values).unwrap()
}

fn profiles(py: Python<'_>, seed: i64) -> Bound<'_, PyList> {
    let rng = module(py, "random")
        .getattr("Random")
        .unwrap()
        .call1((seed,))
        .unwrap();
    let population = module(py, "builtins")
        .getattr("range")
        .unwrap()
        .call1((100,))
        .unwrap();
    let result = PyList::empty(py);
    for index in 0..15 {
        let cluster = index / 5;
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("file", format!("pkg/family_{cluster}/member_{index}.py"))
            .unwrap();
        kwargs.set_item("loc", 120 + index).unwrap();
        kwargs
            .set_item("classes", frozen(py, ["Lane".to_owned()]))
            .unwrap();
        kwargs
            .set_item("function_names", frozen(py, ["run".to_owned()]))
            .unwrap();
        kwargs
            .set_item("method_names", frozen(py, ["forward".to_owned()]))
            .unwrap();
        let hashes = PyDict::new(py);
        hashes
            .set_item("forward", frozen(py, [format!("hash:{cluster}")]))
            .unwrap();
        kwargs.set_item("method_hashes", hashes).unwrap();
        kwargs.set_item("schemas", frozen(py, Vec::new())).unwrap();
        for component in ["structure", "api", "fields", "calls", "control", "imports"] {
            let mut values: Vec<String> = (0..32)
                .map(|slot| format!("{component}:cluster:{cluster}:{slot}"))
                .collect();
            let sample = rng.call_method1("sample", (&population, 5)).unwrap();
            for slot in sample.try_iter().unwrap() {
                let slot: i64 = slot.unwrap().extract().unwrap();
                values.push(format!("{component}:noise:{slot}"));
            }
            kwargs.set_item(component, frozen(py, values)).unwrap();
        }
        result
            .append(
                families(py)
                    .getattr("FileProfile")
                    .unwrap()
                    .call((), Some(&kwargs))
                    .unwrap(),
            )
            .unwrap();
    }
    result
}

fn pairs<'py>(py: Python<'py>) -> Bound<'py, PySet> {
    let expected = PySet::empty(py).unwrap();
    for left in 0..15 {
        for right in left + 1..15 {
            expected.add((left, right)).unwrap();
        }
    }
    expected
}

fn kwargs<'py>(py: Python<'py>, similarity: f64) -> Bound<'py, PyDict> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("min_similarity", similarity).unwrap();
    kwargs.set_item("min_shared_features", 12).unwrap();
    kwargs
}

fn attr<T: for<'a> FromPyObject<'a, 'a>>(item: &Bound<'_, PyAny>, name: &str) -> T {
    item.getattr(name).unwrap().extract().ok().unwrap()
}

#[test]
fn native_pair_scoring_and_safe_bound_match_python() {
    let _case = Case::new();
    Python::attach(|py| {
        let profiles = profiles(py, 20260901);
        let native = families(py);
        let representative = native
            .getattr("compare_profiles")
            .unwrap()
            .call1((profiles.get_item(2).unwrap(), profiles.get_item(3).unwrap()))
            .unwrap();
        assert_eq!(attr::<f64>(&representative, "score"), 0.7769);
        assert_eq!(attr::<f64>(&representative, "containment"), 0.8703);
        assert_eq!(attr::<i64>(&representative, "shared_features"), 161);
        let expected = py_json(
            py,
            json!({
                "structure":0.8049,"api":0.7619,"fields":0.7619,"calls":0.7619,
                "control":0.7619,"imports":0.7619
            }),
        );
        assert!(representative
            .getattr("components")
            .unwrap()
            .eq(expected)
            .unwrap());
        let expected_pairs = pairs(py);
        for threshold in [0.55, 0.70, 0.90] {
            let actual = native
                .getattr("exact_candidate_pairs")
                .unwrap()
                .call((profiles.clone(),), Some(&kwargs(py, threshold)))
                .unwrap();
            let expected = PyTuple::new(
                py,
                [
                    expected_pairs.as_any(),
                    105_i64.into_pyobject(py).unwrap().as_any(),
                ],
            )
            .unwrap();
            assert!(actual.eq(expected).unwrap());
        }
    });
}

fn family_summary(family: &Bound<'_, PyAny>) -> Value {
    let files = family.getattr("files").unwrap();
    let files: Vec<String> = files.cast::<PyList>().unwrap().extract().unwrap();
    json!([
        attr::<String>(family, "id"),
        files,
        attr::<f64>(family, "similarity_min"),
        attr::<f64>(family, "similarity_avg"),
        attr::<f64>(family, "similarity_max"),
        attr::<f64>(family, "containment_min"),
        attr::<i64>(family, "gross_duplicate_loc"),
        attr::<i64>(family, "estimated_net_deleted_loc"),
        attr::<i64>(family, "before_loc"),
        attr::<i64>(family, "after_loc")
    ])
}

#[test]
fn native_complete_link_groups_and_ids_match_python() {
    let _case = Case::new();
    Python::attach(|py| {
        let profiles = profiles(py, 17);
        let native = families(py);
        let exact = native
            .getattr("exact_candidate_pairs")
            .unwrap()
            .call((profiles.clone(),), Some(&kwargs(py, 0.70)))
            .unwrap();
        let family_kwargs = kwargs(py, 0.70);
        family_kwargs.set_item("min_net_deleted_loc", 0).unwrap();
        family_kwargs.set_item("max_family_size", 8).unwrap();
        family_kwargs.set_item("max_candidates", 50).unwrap();
        let actual = native
            .getattr("build_families")
            .unwrap()
            .call((profiles, exact.get_item(0).unwrap()), Some(&family_kwargs))
            .unwrap();
        assert!(actual.get_item(1).unwrap().eq(105).unwrap());
        let families = actual.get_item(0).unwrap();
        let summary: Vec<Value> = families
            .try_iter()
            .unwrap()
            .map(|item| family_summary(&item.unwrap()))
            .collect();
        assert_eq!(
            Value::Array(summary),
            json!([
                [
                    "F001",
                    [
                        "pkg/family_2/member_10.py",
                        "pkg/family_2/member_11.py",
                        "pkg/family_2/member_12.py",
                        "pkg/family_2/member_13.py",
                        "pkg/family_2/member_14.py"
                    ],
                    0.7619,
                    0.7733,
                    0.7855,
                    0.8649,
                    408,
                    381,
                    660,
                    279
                ],
                [
                    "F002",
                    [
                        "pkg/family_1/member_5.py",
                        "pkg/family_1/member_6.py",
                        "pkg/family_1/member_7.py",
                        "pkg/family_1/member_8.py",
                        "pkg/family_1/member_9.py"
                    ],
                    0.7619,
                    0.7688,
                    0.7814,
                    0.8649,
                    392,
                    366,
                    635,
                    269
                ],
                [
                    "F003",
                    [
                        "pkg/family_0/member_0.py",
                        "pkg/family_0/member_1.py",
                        "pkg/family_0/member_2.py",
                        "pkg/family_0/member_3.py",
                        "pkg/family_0/member_4.py"
                    ],
                    0.7619,
                    0.7716,
                    0.7837,
                    0.8649,
                    376,
                    350,
                    610,
                    260
                ]
            ])
        );
    });
}

#[test]
fn explicit_retirement_accounting_ranking_and_validation() {
    let _case = Case::new();
    Python::attach(|py| {
        let globals = PyDict::new(py);
        globals.set_item("families", families(py)).unwrap();
        py.run(
            &CString::new(RETIREMENT_CONTRACTS).unwrap(),
            Some(&globals),
            None,
        )
        .unwrap();
    });
}

const RETIREMENT_CONTRACTS: &str = r#"
import json
from dataclasses import replace
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

with TemporaryDirectory() as directory:
    root = Path(directory)
    source = root / 'plans.json'
    def load(rows):
        source.write_text(json.dumps({'schema_version': 1, 'plans': rows}))
        return families.load_retirement_plans(source)
    def plan(identifier, removed, **extra):
        return dict(id=identifier, remove=removed, **extra)
    rows = [
        plan('reviewed', ['a.py', 'b.py', 'a.py'], add=['shared.py'],
             status='reviewed', reviewer='reviewer', callers=['consumer.py'],
             semantic_constraints=['Keep both mechanisms distinct'],
             provenance=['source revision and reviewed contracts'],
             evidence_ids=['functions:C002', 'functions:C002']),
        plan('planned', ['p1.py', 'p2.py', 'p3.py'], add=['p.py']),
        plan('wrappers', [], retain=['w1.py', 'w2.py'], add=['base.py']),
        plan('config', ['c1.py', 'c2.py', 'c3.py'], add=['config.json'],
             add_configs=['config.json'], add_adapters=['adapter.py']),
    ]
    plans = load(rows)
    evidence = [{'id': 'families:F002', 'files': ['a.py', 'b.py']},
                {'id': 'functions:C002', 'files': ['a.py']},
                {'id': 'functions:C003', 'files': ['b.py']}]
    report = families.retirement_report(plans, evidence)
    assert [row['id'] for row in report['plans']] == ['reviewed', 'planned', 'config', 'wrappers']
    first = report['plans'][0]
    assert (first['before_files'], first['after_files'], first['net_files_removed']) == (2, 1, 1)
    assert first['evidence_ids'] == ['families:F002', 'functions:C002', 'functions:C003']
    assert first['measurement_status'] == 'not_measured'
    assert first['callers'] == ('consumer.py',) and first['provenance']
    assert report['summary']['net_files_removed'] == 3
    assert report['summary']['reviewed_net_files_removed'] == 1
    assert report['plans'][-1]['net_files_removed'] == -1
    config = next(row for row in report['plans'] if row['id'] == 'config')
    assert config['after_files'] == 2  # overlapping add/config entries count once

    for removed_count, expected in [(7, 6), (4, 3), (2, 1)]:
        one = load([plan('compact', [f'old{i}.py' for i in range(removed_count)], add=['new.py'])])
        assert families.retirement_report(one)['summary']['net_files_removed'] == expected

    invalid = [
        [plan('bad', ['../outside.py'])],
        [plan('bad', ['/absolute.py'])],
        [plan('bad', ['a.py'], retain=['a.py'])],
        [plan('bad', ['a.py'], add_configs=['a.py'])],
        [plan('bad', ['a.py'], status='measured')],
        [plan('bad', ['a.py'], status=[])],
        [plan('bad', ['a.py'], status='reviewed')],
        [plan('bad', ['a.py'], ad=['unaccounted.py'])],
        [plan('same', ['a.py']), plan('same', ['b.py'])],
        [plan('one', ['a.py']), plan('two', ['a.py'])],
        [plan('one', ['a.py'], add=['shared.py']), plan('two', ['b.py'], add=['shared.py'])],
    ]
    for items in invalid:
        try:
            load(items)
        except ValueError:
            pass
        else:
            raise AssertionError(f'accepted invalid plan {items}')

    family = families.FileFamily(
        files=['a.py', 'b.py'], similarity_min=.8, similarity_avg=.8,
        similarity_max=.8, containment_min=.9, shared_methods=[], variable_methods=[],
        common_fields=[], recommended_abstraction='shared_module', suggested_home='shared.py',
        gross_duplicate_loc=0, estimated_net_deleted_loc=0, confidence=.8,
        risk='medium', band='80-89%', disposition='validate',
        before_loc=100, after_loc=150, target_shape='2 files to 1', id='F002')
    other = replace(family, files=['other.py', 'another.py'], estimated_net_deleted_loc=1000, id='F001')
    stats = dict(families=2, files_profiled=4, lsh_candidate_pairs=1, pairs_scored=1,
                 estimated_net_deleted_loc=1000)
    plain = families.build_output([other, family], stats, {})
    assert 'retirement_plans' not in plain and plain['families'][0]['net_files_removed'] is None
    reviewed = families.build_output([other, family], stats, {}, plans)
    assert [row['id'] for row in reviewed['families']] == ['F002', 'F001']
    assert reviewed['families'][0]['after_loc'] > reviewed['families'][0]['before_loc']
    assert reviewed['families'][0]['retirement_plan_ids'] == ['reviewed']
    assert reviewed['retirement_summary']['net_files_removed'] == 3
    families.write_markdown(root / 'report.md', reviewed)
    assert 'not measured deletions' in (root / 'report.md').read_text()

    # Explicit plans survive even if the scan emits no families due to its thresholds/caps.
    load(rows)
    empty = {**stats, 'families': 0}
    with patch.object(families, 'scan_file_families', return_value=([], empty)):
        assert families.main(['--repo', str(root), '--targets', '.', '--retirement-plans', str(source),
                              '--out', str(root / 'out.json'), '--md', str(root / 'out.md')]) == 0
    result = json.loads((root / 'out.json').read_text())
    assert len(result['retirement_plans']) == 4 and not result['families']
"#;
