//! Tests for the two configuration rules in `style_scan`.
//!
//! These sit apart from the style tests because they judge a different thing:
//! not how a line is written, but whether a name for something outside this
//! repository is findable. Every exclusion below was measured against the
//! tree's 3303 Python files before it was written -- the false positives are
//! the interesting half, and each one is a shape that really occurs here.

use crate::style_scan::scan_source;

/// Every finding for `source` in `probe.py`, as `rule:line`.
fn rules(source: &str) -> Vec<String> {
    scan_source("probe.py", source)
        .expect("probe parses")
        .into_iter()
        .map(|f| format!("{}:{}", f.rule, f.line))
        .collect()
}

fn fires(source: &str, rule: &str) -> bool {
    rules(source).iter().any(|found| found.starts_with(rule))
}

#[test]
fn a_url_inside_a_call_is_configuration_the_reader_cannot_find() {
    assert!(fires(
        "def probe():\n    return get(\"http://127.0.0.1:7317/v1/embeddings\")\n",
        "config/hardcoded-endpoint"
    ));
}

#[test]
fn a_bare_address_counts_too_because_that_is_how_a_socket_spells_it() {
    // Measured: three files bind or connect to a scheme-less `127.0.0.1`, which
    // a rule keyed on `http` would miss entirely.
    assert!(fires(
        "def serve():\n    run(host=\"0.0.0.0\", port=5000)\n",
        "config/hardcoded-endpoint"
    ));
    assert!(fires(
        "def serve():\n    run(\"127.0.0.1:7317\")\n",
        "config/hardcoded-endpoint"
    ));
}

#[test]
fn a_dotted_name_that_is_not_an_address_is_left_alone() {
    // Four groups, not four numeric octets, and an octet out of range: both are
    // how a version string and a locale look, and neither is an address.
    assert!(!fires(
        "def probe():\n    return get(\"conductor.candidate_review.style_scan\")\n",
        "config/hardcoded-endpoint"
    ));
    assert!(!fires(
        "def probe():\n    return get(\"1.2.3.999\")\n",
        "config/hardcoded-endpoint"
    ));
}

#[test]
fn a_sentence_that_mentions_a_url_is_documentation() {
    // The literal has to *be* the address. An error message naming where to
    // look is prose that happens to contain one.
    assert!(!fires(
        "def probe():\n    raise ValueError(\"see http://127.0.0.1:7317/v1 for the contract\")\n",
        "config/hardcoded-endpoint"
    ));
}

#[test]
fn an_interpolated_url_is_assembled_rather_than_written_down() {
    // The piece in front of the first `{` reads as a whole address on its own
    // -- `http://127.0.0.1:` is a host and a port -- so a scan that takes the
    // first fragment and stops reports a literal nobody wrote.
    assert!(!fires(
        "def probe(port):\n    return get(f\"http://127.0.0.1:{port}/v1\")\n",
        "config/hardcoded-endpoint"
    ));
}

#[test]
fn a_module_level_binding_is_where_the_rule_is_asking_for_it() {
    assert!(!fires(
        "EMBEDDINGS = \"http://127.0.0.1:7317/v1/embeddings\"\n\n\
         def probe():\n    return get(EMBEDDINGS)\n",
        "config/hardcoded-endpoint"
    ));
}

#[test]
fn a_class_level_binding_counts_as_named_but_a_method_body_does_not() {
    // A class attribute is as findable as a module constant. The method beneath
    // it is not: the walk has to stop at the `def`, whatever encloses it.
    assert!(!fires(
        "class Client:\n    BASE = \"https://api.example.com/v2\"\n",
        "config/hardcoded-endpoint"
    ));
    assert!(fires(
        "class Client:\n    def probe(self):\n        return get(\"https://api.example.com/v2\")\n",
        "config/hardcoded-endpoint"
    ));
}

#[test]
fn a_bare_string_statement_is_being_read_not_connected_to() {
    // A string standing on its own as a statement is a docstring or a section
    // marker: the module saying what it is about. Even when the whole literal
    // is the address, nothing connects to it here, and it is already the most
    // findable line in the file.
    assert!(!fires(
        "\"\"\"http://127.0.0.1:7317/v1\"\"\"\n\nimport os\n",
        "config/hardcoded-endpoint"
    ));
}

#[test]
fn a_uuid_in_a_call_is_an_identifier_with_no_name_on_it() {
    assert!(fires(
        "def probe():\n    return load(\"abd64d6f-11b0-4244-b3b7-da26543f3f99\")\n",
        "config/hardcoded-id"
    ));
}

#[test]
fn a_hyphenated_string_shaped_almost_like_a_uuid_is_not_one() {
    // Right number of groups, wrong widths -- which is what a slug looks like.
    assert!(!fires(
        "def probe():\n    return load(\"codex-trident2-credit-data-20260905\")\n",
        "config/hardcoded-id"
    ));
    // Right widths, but `g` is not a hex digit.
    assert!(!fires(
        "def probe():\n    return load(\"abd64d6g-11b0-4244-b3b7-da26543f3f99\")\n",
        "config/hardcoded-id"
    ));
}

#[test]
fn a_written_down_reason_opts_a_literal_out() {
    assert!(!fires(
        "def probe():\n    # guardrail: allow-endpoint\n    return get(\"http://127.0.0.1:7317/v1\")\n",
        "config/hardcoded-endpoint"
    ));
    assert!(!fires(
        "def probe():\n    return load(\"abd64d6f-11b0-4244-b3b7-da26543f3f99\")  # guardrail: allow-id\n",
        "config/hardcoded-id"
    ));
}

#[test]
fn a_test_file_names_its_hosts_and_ids_on_purpose() {
    // The same source under two names: the rule is the path, nothing in the
    // file can say it is test material.
    let source = "def probe():\n    return get(\"https://api.example.com/v2\")\n";
    assert!(!scan_source("audit/tests/test_probe.py", source)
        .expect("probe parses")
        .iter()
        .any(|f| f.rule.starts_with("config/")));
    assert!(scan_source("audit/probe.py", source)
        .expect("probe parses")
        .iter()
        .any(|f| f.rule.starts_with("config/")));
}
