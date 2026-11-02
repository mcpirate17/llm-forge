use super::*;
use std::collections::BTreeMap;

#[test]
fn actual_automatic_campaign_source_matches_cpython_312_symbol_hashes() {
    // Source path is named by campaigns/conductor-67_mutation_coverage_fest_20260913.json.
    // This fixture is the later working source (SHA256 7a17855e...), not the
    // historical source pinned by that manifest; no campaign receipt is inferred.
    let source = include_str!("../tests/fixtures/python_symbol_hash/mutation_coverage.py");
    let expected: BTreeMap<String, String> = serde_json::from_str(include_str!(
        "../tests/fixtures/python_symbol_hash/mutation_coverage_cpython312.json"
    ))
    .unwrap();
    assert_eq!(hashes(source).unwrap(), expected);
}
#[test]
fn cpython_312_type_parameters_decorators_literals_and_methods() {
    let source = include_str!("../tests/fixtures/python_symbol_hash/shapes.py");
    let expected: BTreeMap<String, String> = serde_json::from_str(include_str!(
        "../tests/fixtures/python_symbol_hash/shapes_cpython312.json"
    ))
    .unwrap();
    assert_eq!(hashes(source).unwrap(), expected);
}
#[test]
fn cpython_312_fstrings_bytes_complex_bigints_patterns_and_surrogates() {
    let source = include_str!("../tests/fixtures/python_symbol_hash/advanced.py");
    let expected: BTreeMap<String, String> = serde_json::from_str(include_str!(
        "../tests/fixtures/python_symbol_hash/advanced_cpython312.json"
    ))
    .unwrap();
    assert_eq!(hashes(source).unwrap(), expected);
}
#[test]
fn unhandled_syntax_never_returns_a_partial_map() {
    for source in [
        "def good(): pass\ndef bad(): return t'{1}'\n",
        "def f(): return 1\ntype T[X = int] = X\n",
    ] {
        assert!(
            hashes(source).is_err(),
            "unexpected accepted source: {source}"
        );
    }
    assert!(hashes_with_protocol("def f(): pass\n", "cpython-ast-dump-3.13").is_err());
}
#[test]
fn comments_and_neighboring_symbols_do_not_change_identity() {
    let a = hashes("def f(x):\n return x + 1\ndef g():\n return 2\n").unwrap();
    let b =
        hashes("# comment\ndef f(x):\n # inside\n return x + 1\ndef g():\n return 3\n").unwrap();
    assert_eq!(a["f"], b["f"]);
    assert_ne!(a["g"], b["g"]);
}
