//! `docs/routing.md` cannot drift from what ships: the Policy table rows,
//! the `policy_version` and `tier_order` quoted in the prose, and the exact
//! deny/warn message strings are all asserted here against the embedded
//! `ledger/routing_policy.toml` and the same message fns the hook calls.
//! Drift found by this test is fixed in the docs, never in the policy or
//! the messages.
//!
//! Same `#[path]`-inclusion pattern as the other integration tests in this
//! crate (see tests/ledger_rollup.rs): this binary crate has no lib target,
//! so `extern crate forge` is not an option. The message templates live in
//! pub fns (`inherit_deny_reason`, `above_tier_deny_reason`,
//! `over_cap_reason`, `near_cap_warning`) rather than pub consts because
//! `format!` accepts only literal format strings -- the fn is the single
//! source a const could never be.

#[path = "../src/route.rs"]
#[allow(dead_code)]
mod route;

#[path = "../src/cap_enforce.rs"]
#[allow(dead_code)]
mod cap_enforce;

// Only so the `crate::` paths inside the two modules above resolve:
// route.rs reaches crate::merge, cap_enforce.rs reaches
// crate::subagent_transcript and crate::ledger.
#[path = "../src/merge.rs"]
#[allow(dead_code)]
mod merge;

#[path = "../src/subagent_transcript.rs"]
#[allow(dead_code)]
mod subagent_transcript;

#[path = "../src/ledger/mod.rs"]
#[allow(dead_code)]
mod ledger;

use route::{AgentInput, Policy, Verdict};

const ROUTING_MD: &str = include_str!("../../../docs/routing.md");

/// The docs wrap prose across lines; quoted strings and cells must match
/// modulo whitespace.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Asserts `needle` is quoted somewhere in docs/routing.md, both sides
/// whitespace-normalized.
fn docs_quote(needle: &str) {
    assert!(
        normalize(ROUTING_MD).contains(&normalize(needle)),
        "docs/routing.md does not quote this exact string: {needle:?}"
    );
}

#[test]
fn policy_table_rows_match_the_embedded_policy() {
    let policy = Policy::embedded().expect("embedded routing_policy.toml parses");

    // The `## Policy table` section, up to its first subsection: the
    // markdown table is `| class | matched by | tier | cap_tokens |`.
    let section = ROUTING_MD
        .split("## Policy table")
        .nth(1)
        .and_then(|rest| rest.split("\n### ").next())
        .expect("docs/routing.md has a Policy table section");

    let mut rows = 0;
    for line in section.lines().filter(|l| l.trim_start().starts_with('|')) {
        let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() != 4 {
            continue;
        }
        // Header row and `|---|---|---|---|` separator.
        if cells[0] == "class"
            || cells.iter().all(|c| !c.is_empty() && c.trim_matches('-').is_empty())
        {
            continue;
        }
        // The inherit row renders its tier as a bold `**deny**` plus a
        // qualifier ("unless description contains `justify:`"); the bold
        // span is the tier.
        let tier = cells[2]
            .strip_prefix("**")
            .and_then(|rest| rest.split("**").next())
            .unwrap_or(cells[2]);
        let cap_tokens: u64 = cells[3]
            .parse()
            .unwrap_or_else(|_| panic!("non-numeric cap_tokens in docs row {cells:?}"));
        let Some(rule) = policy.classes.iter().find(|c| c.name == cells[0]) else {
            panic!(
                "docs/routing.md names class {:?} the policy does not define",
                cells[0]
            );
        };
        assert_eq!(tier, rule.tier, "docs tier for class {}", cells[0]);
        assert_eq!(
            cap_tokens, rule.cap_tokens,
            "docs cap_tokens for class {}",
            cells[0]
        );
        rows += 1;
    }
    assert_eq!(
        rows,
        policy.classes.len(),
        "docs table row count vs policy class count (a class added to the \
         policy needs a docs row, and vice versa)"
    );
}

#[test]
fn policy_version_in_prose_matches_the_embedded_policy() {
    let policy = Policy::embedded().expect("embedded routing_policy.toml parses");
    // The prose quotes it as `policy_version = "2026-09-13.1"`.
    let docs = normalize(ROUTING_MD);
    let quoted = docs
        .split("policy_version = \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("docs/routing.md quotes a policy_version in the prose");
    assert_eq!(
        quoted, policy.policy_version,
        "the policy_version quoted in docs/routing.md prose"
    );
}

#[test]
fn tier_order_in_prose_matches_the_embedded_policy() {
    let policy = Policy::embedded().expect("embedded routing_policy.toml parses");
    // The prose: `tier_order` ranks the tiers cheapest to priciest:
    // `haiku < sonnet < opus < fable`.
    let docs = normalize(ROUTING_MD);
    let quoted = docs
        .split("cheapest to priciest: `")
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .expect("docs/routing.md quotes the tier_order ranking");
    let ranked: Vec<&str> = quoted.split(" < ").map(str::trim).collect();
    assert_eq!(
        ranked, policy.tier_order,
        "the tier ranking quoted in docs/routing.md prose"
    );
}

#[test]
fn deny_messages_quoted_in_docs_match_the_route_fns() {
    let policy = Policy::embedded().expect("embedded routing_policy.toml parses");

    // The above-tier deny, rendered through the real decision path so the
    // class_reason wording cannot drift either.
    let above = route::route(
        &policy,
        &AgentInput {
            subagent_type: Some("general-purpose".to_string()),
            requested_model: Some("opus".to_string()),
            description: None,
        },
    );
    assert!(matches!(above.decision, Verdict::Deny));
    docs_quote(&above.reason);
    assert_eq!(
        above.reason,
        route::above_tier_deny_reason(
            "general",
            "subagent_type matches the class",
            "opus",
            "sonnet",
            &policy.justify_marker
        ),
        "route() must compose its deny reason via above_tier_deny_reason"
    );

    // The inherit deny, same real-decision rendering.
    let inherit = route::route(
        &policy,
        &AgentInput {
            subagent_type: Some("fork".to_string()),
            requested_model: None,
            description: None,
        },
    );
    assert!(matches!(inherit.decision, Verdict::Deny));
    docs_quote(&inherit.reason);
    assert_eq!(
        inherit.reason,
        route::inherit_deny_reason(
            "inherit",
            "subagent_type matches the inherit class",
            &policy.justify_marker
        ),
        "route() must compose its deny reason via inherit_deny_reason"
    );
}

#[test]
fn cap_messages_quoted_in_docs_match_the_cap_fns() {
    // The numbers are the docs' own examples (a 150000-token general-class
    // cap at 120000 and 162345 billed); the fns are the hook's templates.
    docs_quote(&cap_enforce::near_cap_warning(150000, "general", 120000));
    docs_quote(&cap_enforce::over_cap_reason(150000, "general", 162345));
}
