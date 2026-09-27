#![cfg(feature = "python-compat-tests")]
//! Rust-owned mechanical-mutant and disposable-campaign contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/uncurated_kill_rate_support.rs"]
mod uncurated_support;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use support::{module, path, AttrPatch, Case};
use uncurated_support::{api, details, fixture, generate, measure, sources, SUBJECT};

#[test]
fn a_comparison_is_flipped_to_its_boundary_neighbour() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(sources(py, "def f(n):\n    return n < 1\n", None)
            .iter()
            .any(|s| s.contains("n <= 1")))
    });
}

#[test]
fn membership_and_identity_comparisons_are_flipped() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(details(py, "def f(a, b):\n    return a in b\n", None)
            .contains(&"In->NotIn".to_owned()));
        assert!(details(py, "def f(a, b):\n    return a is b\n", None)
            .contains(&"Is->IsNot".to_owned()));
    });
}

#[test]
fn a_boolean_operator_is_flipped() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(sources(py, "def f(a, b):\n    return a and b\n", None)
            .iter()
            .any(|s| s.contains("a or b")))
    });
}

#[test]
fn a_negation_is_dropped() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(sources(py, "def f(a):\n    return not a\n", None)
            .iter()
            .any(|s| s.trim().ends_with("return a")))
    });
}

#[test]
fn arithmetic_is_swapped() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(
            details(py, "def f(a, b):\n    return a + b\n", None).contains(&"Add->Sub".to_owned())
        )
    });
}

#[test]
fn an_augmented_assignment_is_swapped() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(details(py, "def f(a):\n    a += 1\n    return a\n", None)
            .contains(&"Add->Sub".to_owned()))
    });
}

#[test]
fn a_boolean_constant_is_inverted() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(sources(py, "def f():\n    return True\n", None)
            .iter()
            .any(|s| s.contains("return False")))
    });
}

#[test]
fn an_integer_constant_is_perturbed() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(sources(py, "def f():\n    return 7\n", None)
            .iter()
            .any(|s| s.contains("return 8")))
    });
}

#[test]
fn a_string_constant_is_emptied() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(sources(py, "def f():\n    return 'abc'\n", None)
            .iter()
            .any(|s| s.contains("return ''")))
    });
}

#[test]
fn a_returned_value_is_replaced_with_none() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(sources(py, "def f(a):\n    return a\n", None)
            .iter()
            .any(|s| s.lines().next_back().unwrap().trim() == "return"));
        let rows = generate(py, "def f():\n    return None\n", "s.py", &[1, 2]);
        assert!(!rows
            .iter()
            .any(|row| row.getattr("op").unwrap().eq("return-none").unwrap()));
    });
}

#[test]
fn loop_exits_are_exchanged() {
    let _case = Case::new();
    Python::attach(|py| {
        let source =
            "def f(xs):\n    for x in xs:\n        if x:\n            continue\n        break\n";
        let found = details(py, source, None);
        assert!(found.contains(&"continue -> break".to_owned()));
        assert!(found.contains(&"break -> continue".to_owned()));
    });
}

#[test]
fn an_uncovered_line_is_never_mutated() {
    let _case = Case::new();
    Python::attach(|py| {
        let source = "def f(a, b):\n    if a < b:\n        return 1\n    return 2\n";
        assert_eq!(sources(py, source, Some(&[])), Vec::<String>::new());
        let rows = generate(py, source, "subject.py", &[2]);
        assert!(!rows.is_empty());
        let lines: BTreeSet<i64> = rows
            .iter()
            .map(|m| m.getattr("line").unwrap().extract().unwrap())
            .collect();
        assert_eq!(lines, BTreeSet::from([2]));
    });
}

#[test]
fn output_is_well_formed_and_free_of_duplicates() {
    let _case = Case::new();
    Python::attach(|py| {
        let source =
            "def f(a, b):\n    if a < b and not not a:\n        return 'x'\n    return a + 1\n";
        let ast = module(py, "ast");
        let original: String = ast
            .getattr("unparse")
            .unwrap()
            .call1((ast.getattr("parse").unwrap().call1((source,)).unwrap(),))
            .unwrap()
            .extract()
            .unwrap();
        let mutants = sources(py, source, None);
        assert!(!mutants.is_empty());
        assert!(!mutants.contains(&original));
        assert_eq!(mutants.iter().collect::<BTreeSet<_>>().len(), mutants.len());
        let compile = module(py, "builtins").getattr("compile").unwrap();
        for text in mutants {
            compile.call1((text, "subject.py", "exec")).unwrap();
        }
    });
}

#[test]
fn the_operator_label_names_what_changed() {
    let _case = Case::new();
    Python::attach(|py| {
        let compare = generate(
            py,
            "def f(a, b):\n    if a < b:\n        pass\n",
            "s.py",
            &[1, 2, 3],
        );
        let labels: BTreeSet<String> = compare
            .iter()
            .map(|m| m.getattr("op").unwrap().extract().unwrap())
            .collect();
        assert_eq!(labels, BTreeSet::from(["compare".to_owned()]));
        let constant = generate(py, "def f():\n    x = True\n", "s.py", &[1, 2]);
        let labels: BTreeSet<String> = constant
            .iter()
            .map(|m| m.getattr("op").unwrap().extract().unwrap())
            .collect();
        assert_eq!(labels, BTreeSet::from(["const-bool".to_owned()]));
    });
}

#[test]
fn each_mutant_changes_exactly_one_site() {
    let _case = Case::new();
    Python::attach(|py| {
        let source = "def f(a, b):\n    if a < b:\n        return 1\n    return a + 1\n";
        let ast = module(py, "ast");
        let baseline: String = ast
            .getattr("unparse")
            .unwrap()
            .call1((ast.getattr("parse").unwrap().call1((source,)).unwrap(),))
            .unwrap()
            .extract()
            .unwrap();
        let mutants = sources(py, source, None);
        assert!(!mutants.is_empty());
        for mutant in mutants {
            let lines: Vec<_> = mutant.lines().collect();
            let original: Vec<_> = baseline.lines().collect();
            assert_eq!(lines.len(), original.len());
            let differing = lines
                .iter()
                .zip(original.iter())
                .filter(|(a, b)| a != b)
                .count();
            assert_eq!(differing, 1, "{mutant}");
        }
    });
}

#[test]
fn measure_counts_by_exit_code_and_restores_the_tree() {
    let case = Case::new();
    let manifest = fixture(case.root(), json!({"subject.py":"","test_subject.py":""}));
    Python::attach(|py| {
        let result = measure(py, case.root(), &manifest);
        let count = |name: &str| -> i64 { result.getattr(name).unwrap().extract().unwrap() };
        assert_eq!(
            count("ran"),
            count("killed") + count("survived") + count("timeout") + count("broken")
        );
        assert_eq!(count("broken"), 0);
        let survivor = PyDict::new(py);
        survivor.set_item("path", "subject.py").unwrap();
        survivor.set_item("line", 2).unwrap();
        survivor.set_item("op", "const-str").unwrap();
        survivor.set_item("detail", "'unread'->''").unwrap();
        let expected = PyList::new(py, [survivor]).unwrap();
        assert!(result.getattr("survivors").unwrap().eq(expected).unwrap());
        assert!(count("killed") >= 4);
    });
    assert_eq!(
        fs::read_to_string(case.root().join("subject.py")).unwrap(),
        SUBJECT
    );
}

#[test]
fn a_test_file_is_never_a_mutation_subject() {
    let case = Case::new();
    let manifest = fixture(case.root(), json!({"test_subject.py":""}));
    Python::attach(|py| {
        let result = measure(py, case.root(), &manifest);
        assert!(result.getattr("generated").unwrap().eq(0).unwrap());
        assert!(result.getattr("ran").unwrap().eq(0).unwrap());
        assert!(result.getattr("baseline_seconds").unwrap().eq(0.0).unwrap());
    });
}

#[test]
fn cli_writes_a_row_for_every_campaign() {
    let case = Case::new();
    let campaigns = case.mkdir("conductor/mutation_campaigns");
    let manifest = fixture(case.root(), json!({"test_subject.py":""}));
    for name in ["alpha", "beta"] {
        fs::copy(&manifest, campaigns.join(format!("{name}.json"))).unwrap();
    }
    let out = case.root().join("results.json");
    Python::attach(|py| {
        let argv = PyList::new(
            py,
            [
                "uncurated_kill_rate",
                "--root",
                case.root().to_str().unwrap(),
                "--campaign",
                "alpha",
                "--campaign",
                "beta",
                "--out",
                out.to_str().unwrap(),
            ],
        )
        .unwrap();
        let sys = module(py, "sys");
        let _patch = AttrPatch::replace(&sys.into_any(), "argv", &argv.into_any());
        assert!(api(py)
            .getattr("main")
            .unwrap()
            .call0()
            .unwrap()
            .eq(0)
            .unwrap());
    });
    let rows: serde_json::Value = serde_json::from_str(&fs::read_to_string(out).unwrap()).unwrap();
    assert_eq!(
        rows.as_array()
            .unwrap()
            .iter()
            .map(|row| row["campaign"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
}

#[test]
fn campaign_resolves_by_the_id_its_manifest_declares() {
    let case = Case::new();
    let campaigns = case.mkdir("conductor/mutation_campaigns");
    let manifest = fixture(case.root(), json!({"test_subject.py":""}));
    let mut payload: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
    payload["campaign_id"] = json!("declared_id_v4");
    let named = campaigns.join("on_disk_name.json");
    fs::write(&named, serde_json::to_string(&payload).unwrap()).unwrap();
    Python::attach(|py| {
        let result = api(py)
            .getattr("resolve_campaigns")
            .unwrap()
            .call1((
                path(py, case.root()),
                PyList::new(py, ["declared_id_v4"]).unwrap(),
            ))
            .unwrap();
        let expected = PyList::new(
            py,
            [PyTuple::new(
                py,
                [
                    "declared_id_v4".into_pyobject(py).unwrap().into_any(),
                    path(py, &named),
                ],
            )
            .unwrap()],
        )
        .unwrap();
        assert!(result.eq(expected).unwrap());
    });
}

#[test]
fn unresolvable_campaign_is_refused_before_measurement() {
    let case = Case::new();
    let campaigns = case.mkdir("conductor/mutation_campaigns");
    let manifest = fixture(case.root(), json!({"subject.py":"","test_subject.py":""}));
    fs::copy(&manifest, campaigns.join("alpha.json")).unwrap();
    let out = case.root().join("results.json");
    Python::attach(|py| {
        let argv = PyList::new(
            py,
            [
                "uncurated_kill_rate",
                "--root",
                case.root().to_str().unwrap(),
                "--campaign",
                "alpha",
                "--campaign",
                "nope",
                "--campaign",
                "also_nope",
                "--out",
                out.to_str().unwrap(),
            ],
        )
        .unwrap();
        let sys = module(py, "sys");
        let _patch = AttrPatch::replace(&sys.into_any(), "argv", &argv.into_any());
        let api = api(py);
        let error = api.getattr("main").unwrap().call0().unwrap_err();
        assert!(error
            .matches(py, &api.getattr("CampaignLookupError").unwrap())
            .unwrap());
        let message = error.to_string();
        assert!(message.contains("nope"));
        assert!(message.contains("also_nope"));
    });
    assert!(!out.exists());
}

#[test]
fn campaign_that_executes_no_subject_is_a_row_not_a_crash() {
    let case = Case::new();
    fs::write(
        case.root().join("subject.py"),
        "def unused():\n    return 1\n",
    )
    .unwrap();
    fs::write(
        case.root().join("test_subject.py"),
        "def test_nothing():\n    assert True\n",
    )
    .unwrap();
    let manifest = case.root().join("campaign.json");
    let payload = json!({"source_sha256":{"subject.py":"","test_subject.py":""},
        "baseline":{"argv":["python","-m","pytest","-q","-o","addopts=","--rootdir=.","test_subject.py"]}});
    fs::write(&manifest, serde_json::to_string(&payload).unwrap()).unwrap();
    Python::attach(|py| {
        let result = measure(py, case.root(), &manifest);
        assert!(result.getattr("ran").unwrap().eq(0).unwrap());
        assert!(result.getattr("generated").unwrap().eq(0).unwrap());
        assert!(result
            .getattr("note")
            .unwrap()
            .eq("coverage measurement failed")
            .unwrap());
    });
}
