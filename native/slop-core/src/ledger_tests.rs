//! What the backlog must get right: one row per unit of work, the undriven first,
//! and an id that survives the file being edited around it.

use crate::ledger::{aggregate, diff, tally, Finding};

fn f(module: &str, qualname: &str, verdict: &str, rule: &str, line: usize) -> Finding {
    Finding {
        module: module.into(),
        qualname: qualname.into(),
        verdict: verdict.into(),
        rule: rule.into(),
        line,
        description: format!("{rule} removed"),
    }
}

#[test]
fn findings_collapse_to_one_item_per_function_and_verdict() {
    // The whole reason this module exists: the 2026-08-31 sweep emitted 2,776 findings
    // over 931 functions, so two thirds of a flat report is the same function repeated
    // with a different rule attached.
    let items = aggregate(&[
        f(
            "conductor/a.py",
            "run",
            "NOT_EXERCISED",
            "drop_raise_guard",
            10,
        ),
        f("conductor/a.py", "run", "NOT_EXERCISED", "drop_clamp", 14),
        f("conductor/a.py", "run", "NOT_EXERCISED", "drop_detach", 12),
    ]);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].findings, 3);
    assert_eq!(
        items[0].rules,
        vec!["drop_clamp", "drop_detach", "drop_raise_guard"]
    );
}

#[test]
fn one_function_with_two_verdicts_is_two_items() {
    // "nothing tests this" and "this appears to do nothing" are different pieces of
    // work with different fixes, even on the same function.
    let items = aggregate(&[
        f("conductor/a.py", "run", "NOT_EXERCISED", "drop_clamp", 10),
        f(
            "conductor/a.py",
            "run",
            "NO_DIFFERENCE_OBSERVED",
            "drop_clamp",
            10,
        ),
    ]);
    assert_eq!(items.len(), 2);
    assert_ne!(items[0].id, items[1].id);
}

#[test]
fn an_item_opens_at_the_earliest_line_of_its_findings() {
    let items = aggregate(&[
        f("conductor/a.py", "run", "NOT_EXERCISED", "b", 40),
        f("conductor/a.py", "run", "NOT_EXERCISED", "a", 12),
    ]);
    assert_eq!(items[0].line, 12);
}

#[test]
fn an_undriven_construct_outranks_an_uncalled_function_regardless_of_volume() {
    // Sorted by volume alone, fifty advisory findings on one helper would bury the one
    // construct that changes behaviour where no test looks.
    let mut findings = vec![f("b.py", "run", "NOT_EXERCISED", "r", 1)];
    for i in 0..50 {
        findings.push(f(
            "a.py",
            "helper",
            "NO_DIFFERENCE_OBSERVED",
            &format!("r{i}"),
            1,
        ));
    }
    findings.push(f("c.py", "step", "REACHABLE_BUT_UNTESTED", "r", 1));
    let items = aggregate(&findings);
    let order: Vec<&str> = items.iter().map(|i| i.verdict.as_str()).collect();
    assert_eq!(
        order,
        [
            "REACHABLE_BUT_UNTESTED",
            "NOT_EXERCISED",
            "NO_DIFFERENCE_OBSERVED"
        ]
    );
    assert_eq!(items[2].findings, 50);
}

#[test]
fn the_id_ignores_the_line_number() {
    // Lines move whenever anything above them is edited. An id that moved with them
    // would report one item fixed and another appearing every time a docstring grew,
    // and a burndown built on that measures editing, not progress.
    let a = aggregate(&[f("conductor/a.py", "run", "NOT_EXERCISED", "r", 10)]);
    let b = aggregate(&[f("conductor/a.py", "run", "NOT_EXERCISED", "r", 900)]);
    assert_eq!(a[0].id, b[0].id);
}

#[test]
fn renaming_a_function_mints_a_new_id() {
    let a = aggregate(&[f("conductor/a.py", "run", "NOT_EXERCISED", "r", 1)]);
    let b = aggregate(&[f("conductor/a.py", "run_v2", "NOT_EXERCISED", "r", 1)]);
    assert_ne!(a[0].id, b[0].id);
}

#[test]
fn ids_distinguish_the_same_function_name_in_different_modules() {
    let items = aggregate(&[
        f("conductor/a.py", "run", "NOT_EXERCISED", "r", 1),
        f("conductor/b.py", "run", "NOT_EXERCISED", "r", 1),
    ]);
    assert_ne!(items[0].id, items[1].id);
}

#[test]
fn the_diff_separates_new_from_carried_and_reports_what_went_away() {
    let items = aggregate(&[
        f("conductor/a.py", "run", "NOT_EXERCISED", "r", 1),
        f("conductor/b.py", "run", "NOT_EXERCISED", "r", 1),
    ]);
    let known_a = items
        .iter()
        .find(|i| i.module == "conductor/a.py")
        .unwrap()
        .id
        .clone();
    let d = diff(&[known_a.clone(), "deadbeefdeadbeef".to_string()], &items);
    assert_eq!(d.carried.len(), 1);
    assert_eq!(d.carried[0].id, known_a);
    assert_eq!(d.new.len(), 1);
    assert_eq!(d.new[0].module, "conductor/b.py");
    assert_eq!(d.fixed, vec!["deadbeefdeadbeef".to_string()]);
}

#[test]
fn a_first_run_reports_everything_new_and_nothing_fixed() {
    let items = aggregate(&[f("conductor/a.py", "run", "NOT_EXERCISED", "r", 1)]);
    let d = diff(&[], &items);
    assert_eq!(d.new.len(), 1);
    assert!(d.carried.is_empty());
    assert!(d.fixed.is_empty());
}

#[test]
fn the_ordering_is_total_so_two_runs_of_one_tree_diff_clean() {
    let mk = || {
        vec![
            f("conductor/b.py", "z", "NOT_EXERCISED", "r", 1),
            f("conductor/a.py", "z", "NOT_EXERCISED", "r", 1),
            f("conductor/a.py", "a", "NOT_EXERCISED", "r", 1),
        ]
    };
    let one: Vec<String> = aggregate(&mk()).iter().map(|i| i.id.clone()).collect();
    let mut shuffled = mk();
    shuffled.reverse();
    let two: Vec<String> = aggregate(&shuffled).iter().map(|i| i.id.clone()).collect();
    assert_eq!(one, two);
}

#[test]
fn the_tally_counts_items_not_findings() {
    let items = aggregate(&[
        f("conductor/a.py", "run", "NOT_EXERCISED", "r1", 1),
        f("conductor/a.py", "run", "NOT_EXERCISED", "r2", 1),
        f("research/tools/x.py", "h", "NO_DIFFERENCE_OBSERVED", "r", 1),
    ]);
    let t = tally(&items);
    assert_eq!(t["NOT_EXERCISED"], 1);
    assert_eq!(t["NO_DIFFERENCE_OBSERVED"], 1);
}
