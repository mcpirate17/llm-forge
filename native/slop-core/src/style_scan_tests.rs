//! Each test pins one rule against the case that made the rule necessary, or
//! against the false positive that made it narrower. The exclusions are the
//! interesting half: every one of them was measured on this repository's 3301
//! Python files before it was written.

use crate::style_scan::scan_source;

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
fn a_first_person_subject_up_front_is_narration() {
    assert!(fires(
        "# Now we walk the list and total it up.\nx = 1\n",
        "comment/effort-narrative"
    ));
}

#[test]
fn a_pronoun_late_in_a_sentence_is_ordinary_prose() {
    // Measured: this exact comment appears twice in the tree and is a
    // description of behaviour, not narration.
    assert!(!fires(
        "# Load phase checkpoint to find where we left off\nx = 1\n",
        "comment/effort-narrative"
    ));
}

#[test]
fn an_edit_verb_opens_a_comment_about_the_commit() {
    assert!(fires(
        "# Added this to handle the empty case.\nx = 1\n",
        "comment/change-meta"
    ));
}

#[test]
fn a_dated_removal_note_is_a_decision_record() {
    // "REMOVED 2026-08-02 (user directive): ..." is archaeology this repo wants
    // kept; undated edit chatter is what the rule is for.
    assert!(!fires(
        "# REMOVED 2026-08-02 (user directive): the softmax twin.\nx = 1\n",
        "comment/change-meta"
    ));
}

#[test]
fn fixed_is_read_as_an_adjective_not_a_repair() {
    assert!(!fires(
        "# Fixed seed: identical across machines.\nx = 1\n",
        "comment/change-meta"
    ));
}

#[test]
fn a_comment_adding_no_word_the_code_lacks_is_a_restatement() {
    assert!(fires(
        "# Sum totals\nsum_totals = value\n",
        "comment/trivial-restatement"
    ));
}

#[test]
fn a_snake_case_name_is_compared_word_by_word() {
    // `record_routing_telemetry` has to read as three words, or a comment
    // restating it in three words looks like new information.
    assert!(fires(
        "# Record routing telemetry\nrecord_routing_telemetry(state)\n",
        "comment/trivial-restatement"
    ));
}

#[test]
fn a_comment_carrying_one_new_word_is_not_a_restatement() {
    assert!(!fires(
        "# Sum totals once the guard passes\nsum_totals = value\n",
        "comment/trivial-restatement"
    ));
}

#[test]
fn a_section_heading_is_not_judged_against_the_line_below_it() {
    assert!(!fires(
        "# -- Unary ops --\nunary_ops = build()\n",
        "comment/trivial-restatement"
    ));
}

#[test]
fn shape_notation_is_not_prose() {
    assert!(!fires(
        "# x: (B, S, D) -> (B, S, 1, D)\nx = reshape(x)\n",
        "comment/trivial-restatement"
    ));
}

#[test]
fn a_paragraph_is_an_explanation_not_a_label() {
    // Only the opening line of a comment block can be narration, and a
    // continuation line restating the code below is just how prose wraps.
    assert!(!fires(
        "# The totals are summed here because the caller needs them eagerly.\n\
         # Sum totals\nsum_totals = value\n",
        "comment/trivial-restatement"
    ));
}

#[test]
fn machine_directives_and_tracked_debt_are_never_read() {
    for comment in [
        "# noqa: E501",
        "# type: ignore[arg-type]",
        "# TODO: we will rewrite this",
        "# FIXME: added a hack here",
    ] {
        assert!(
            rules(&format!("{comment}\nx = 1\n")).is_empty(),
            "{comment} should be ignored"
        );
    }
}

#[test]
fn a_body_that_supplies_nothing_is_an_unfinished_stub() {
    assert!(fires("def stub(value):\n    pass\n", "dead/empty-function"));
    assert!(fires("def stub(value):\n    ...\n", "dead/empty-function"));
    assert!(fires(
        "def stub(value):\n    \"\"\"Docs only.\"\"\"\n",
        "dead/empty-function"
    ));
}

#[test]
fn a_no_op_the_author_explained_is_a_decision() {
    assert!(!fires(
        "def close(self):\n    # The connection must outlive this object.\n    pass\n",
        "dead/empty-function"
    ));
}

#[test]
fn an_abstract_or_overloaded_method_is_meant_to_be_empty() {
    assert!(!fires(
        "class A:\n    @abstractmethod\n    def run(self):\n        ...\n",
        "dead/empty-function"
    ));
    assert!(!fires(
        "class A:\n    @overload\n    def run(self):\n        ...\n",
        "dead/empty-function"
    ));
}

#[test]
fn a_protocol_member_is_a_signature_not_a_stub() {
    assert!(!fires(
        "class Reader(Protocol):\n    def read(self) -> str:\n        ...\n",
        "dead/empty-function"
    ));
}

#[test]
fn the_escape_hatch_waives_the_definition_beside_it() {
    assert!(!fires(
        "def stub(value):  # guardrail: allow-stub\n    pass\n",
        "dead/empty-function"
    ));
}

#[test]
fn a_statement_after_a_terminator_is_dead() {
    let found = rules("def f():\n    return 1\n    print(2)\n");
    assert!(found.contains(&"dead/unreachable-statement:3".to_string()));
}

#[test]
fn a_terminator_that_ends_its_block_leaves_nothing_behind() {
    assert!(!fires(
        "def f():\n    if x:\n        return 1\n    return 2\n",
        "dead/unreachable-statement"
    ));
}

#[test]
fn a_trailing_comment_is_not_unreachable_code() {
    assert!(!fires(
        "def f():\n    return 1\n    # kept for the next reader\n",
        "dead/unreachable-statement"
    ));
}

#[test]
fn a_literal_condition_decides_the_branch_before_the_run() {
    assert!(fires("if True:\n    x = 1\n", "dead/constant-condition"));
}

#[test]
fn while_true_is_the_idiomatic_loop_not_a_dead_branch() {
    assert!(!fires(
        "while True:\n    x = 1\n    break\n",
        "dead/constant-condition"
    ));
    assert!(fires(
        "while False:\n    x = 1\n",
        "dead/constant-condition"
    ));
}

#[test]
fn unparsable_python_is_reported_rather_than_returned_clean() {
    // Returning no findings for a file the scanner could not read would be a
    // silent pass on exactly the file most likely to be broken.
    assert!(scan_source("probe.py", "def (:\n").is_err());
}

#[test]
fn the_published_rule_list_matches_what_the_rules_emit() {
    let published = crate::style_scan::style_scan_rules();
    let source = "# Now we added this\ndef stub(v):\n    pass\n\n\
                  # Sum totals\nsum_totals = v\n\ndef g():\n    if True:\n        return 1\n    \
                  return 2\n    print(3)\n";
    for finding in scan_source("probe.py", source).expect("probe parses") {
        assert!(
            published.contains(&finding.rule),
            "{} is emitted but not published",
            finding.rule
        );
    }
    assert!(published.windows(2).all(|w| w[0] < w[1]), "list is sorted");
}
