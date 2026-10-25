//! One test per trigger, one per exclusion that keeps a non-array loop quiet,
//! and the waiver.

use crate::hot_loop_scan::{in_hot_path, scan_source};

fn lines(source: &str) -> Vec<usize> {
    scan_source("probe.py", source)
        .expect("probe parses")
        .into_iter()
        .map(|f| f.line)
        .collect()
}

#[test]
fn a_range_over_len_of_a_list_is_quiet() {
    assert!(lines("def f(x):\n    for i in range(len(x)):\n        pass\n").is_empty());
}

#[test]
fn a_chunked_range_that_slices_a_batch_is_quiet() {
    let source =
        "def f(n):\n    t = torch.zeros(n)\n    for s in range(0, t.shape[0], 8):\n        b = t[s:s + 8]\n";
    assert!(lines(source).is_empty());
}

#[test]
fn indexing_that_ignores_the_loop_variable_is_quiet() {
    let source = "def f(n):\n    t = torch.zeros(n)\n    for i in range(n):\n        print(t[0])\n";
    assert!(lines(source).is_empty());
}

#[test]
fn a_training_step_loop_is_quiet() {
    let source = "def f(n):\n    t = torch.zeros(n)\n    for i in range(n):\n        loss = t[i]\n        loss.backward()\n";
    assert!(lines(source).is_empty());
}

#[test]
fn a_range_over_len_is_flagged() {
    let source = "def f():\n    x = np.zeros(3)\n    for i in range(len(x)):\n        pass\n";
    assert_eq!(lines(source), [3]);
}

#[test]
fn a_range_over_shape_size_or_numel_is_flagged() {
    for bound in ["x.shape[0]", "x.size(0)", "x.numel()"] {
        let source = format!("def f(x):\n    for i in range({bound}):\n        pass\n");
        assert_eq!(lines(&source), [2], "{bound}");
    }
}

#[test]
fn a_loop_over_a_name_bound_from_a_torch_call_is_flagged() {
    let source =
        "import torch\ndef f():\n    t = torch.zeros(4)\n    for row in t:\n        pass\n";
    assert_eq!(lines(source), [4]);
}

#[test]
fn a_loop_over_a_name_bound_from_a_numpy_call_is_flagged() {
    let source = "import numpy as np\ndef f():\n    a = np.arange(4)\n    for i, v in enumerate(a):\n        pass\n";
    assert_eq!(lines(source), [4]);
}

#[test]
fn a_parameter_annotated_as_a_tensor_is_an_array() {
    let source = "def f(x: torch.Tensor):\n    for row in x:\n        pass\n";
    assert_eq!(lines(source), [2]);
}

#[test]
fn indexing_an_array_in_the_body_is_flagged() {
    let source = "def f(n):\n    t = np.zeros(n)\n    for i in range(n):\n        t[i] = 1\n";
    assert_eq!(lines(source), [3]);
}

#[test]
fn slicing_an_array_in_a_comprehension_is_flagged() {
    let source = "def f(n):\n    t = torch.zeros(n)\n    return [t[i:i + 2] for i in range(n)]\n";
    assert_eq!(lines(source), [3]);
}

#[test]
fn item_tolist_and_numpy_per_iteration_are_flagged() {
    for pull in ["t.item()", "t.tolist()", "t.numpy()"] {
        let source = format!(
            "def f(n):\n    t = torch.zeros(n)\n    for v in range(n):\n        print({pull})\n"
        );
        assert_eq!(lines(&source), [3], "{pull}");
    }
}

#[test]
fn tolist_on_a_value_not_shown_to_be_an_array_is_quiet() {
    let source = "def f(rows):\n    for v in rows:\n        print(v.tolist())\n";
    assert!(lines(source).is_empty());
}

#[test]
fn a_while_bounded_by_len_is_flagged() {
    let source =
        "def f():\n    x = torch.ones(3)\n    i = 0\n    while i < len(x):\n        i += 1\n";
    assert_eq!(lines(source), [4]);
}

#[test]
fn a_loop_over_a_list_of_strings_is_quiet() {
    let source = "def f(names: list[str]):\n    for name in names:\n        print(name)\n";
    assert!(lines(source).is_empty());
}

#[test]
fn a_loop_over_file_paths_is_quiet() {
    let source = "def f(root):\n    for path in root.glob('*.py'):\n        print(path.name)\n";
    assert!(lines(source).is_empty());
}

#[test]
fn a_range_of_a_constant_with_no_array_use_is_quiet() {
    assert!(lines("def f():\n    for i in range(10):\n        print(i)\n").is_empty());
}

#[test]
fn iterating_a_shape_tuple_is_quiet() {
    assert!(lines("def f(x):\n    for d in x.shape:\n        print(d)\n").is_empty());
}

#[test]
fn a_name_bound_from_a_scalar_method_is_not_an_array() {
    let source = "def f(x):\n    n = np.prod(x).item()\n    for i in n:\n        pass\n";
    assert!(lines(source).is_empty());
}

#[test]
fn an_array_in_another_function_does_not_leak_in() {
    let source = "def g():\n    t = torch.zeros(3)\n\ndef f(t):\n    for row in t:\n        pass\n";
    assert!(lines(source).is_empty());
}

#[test]
fn the_waiver_on_the_loop_or_above_it_silences_the_loop() {
    let above = "def f(x):\n    # guardrail: allow-python-loop: 3 layers\n    for i in range(len(x)):\n        pass\n";
    let same = "def f(x):\n    for i in range(len(x)):  # guardrail: allow-python-loop: 3 layers\n        pass\n";
    assert!(lines(above).is_empty());
    assert!(lines(same).is_empty());
}

#[test]
fn invalid_python_is_an_error_not_a_pass() {
    assert!(scan_source("probe.py", "def f(:\n").is_err());
}

#[test]
fn only_python_files_under_a_listed_folder_are_in_scope() {
    let folders = vec!["research/runtime".to_string(), "aria_core/".to_string()];
    assert!(in_hot_path("research/runtime/a/b.py", &folders));
    assert!(in_hot_path("aria_core/x.py", &folders));
    assert!(!in_hot_path("research/runtime_extra/b.py", &folders));
    assert!(!in_hot_path("research/runtime/notes.md", &folders));
    assert!(!in_hot_path("tests/b.py", &folders));
    assert!(!in_hot_path("research/runtime/tests/b.py", &folders));
    assert!(!in_hot_path("research/runtime/test_b.py", &folders));
    assert!(!in_hot_path("research/runtime/b.py", &[]));
}
