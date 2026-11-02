use super::super::LIMITS;
use super::*;

fn good(text: &str) {
    assert_eq!(validate(text, LIMITS), Ok(()), "{text}");
}
fn bad(text: &str) {
    assert!(validate(text, LIMITS).is_err(), "{text}");
}
#[test]
fn ordinary_yaml_strings_numbers_keys_and_duplicates_remain_valid() {
    for text in [
        "release: 2026-latest\n12-service: 01\n09: ordinary string\n",
        "value: 0b101\noctal: 010\nhex: 0xFF\nsexagesimal: 1:20\n",
        "floats: [1.2, 1.0e+3, .5, 1:20.5, .inf, -.Inf, .NaN]\n",
        "key: a\nkey: b\non: first\ntrue: second\n1: first\n1.0: second\n",
        "? 2026-11-01\n: date key\n? .nan\n: floating key\n",
        "'<<': data\n'=': data\n'2026-99-99': data\n'01': data\n",
    ] {
        good(text);
    }
}
#[test]
fn alias_graph_and_merge_maps_are_valid_without_alias_expansion() {
    for text in [
        "defaults: &d {timeout: 3, enabled: true}\njob: {<<: *d, timeout: 4}\n",
        "a: &a {x: 1}\nb: &b {y: 2}\nc: {<<: [*a, *b], z: 3}\n",
        "a: &a [*a]\n",
        "a: &a {self: *a}\n",
        "a: &a scalar\nb: [*a, *a]\n",
        "? &value =\n: first\nalias: *value\n",
    ] {
        good(text);
    }
    let mut text = "a0: &a0 [leaf]\n".to_string();
    for i in 1..25 {
        text.push_str(&format!("a{i}: &a{i} [*a{}, *a{}]\n", i - 1, i - 1));
    }
    let graph = Graph::parse(&text, LIMITS).unwrap();
    assert!(
        graph.nodes.len() < 100,
        "alias edges must not clone subtrees"
    );
    good(&text);
}
#[test]
fn safe_collection_and_scalar_tags_validate_constructor_shapes() {
    for text in [
        "!!map {a: !!int '01', b: !!float '1e3', c: !!bool 'YES'}",
        "!!seq [!!str 01, !!null anything, !!binary 'SGVsbG8=']",
        "!!set {a: null, b: null}",
        "!!omap [a: 1, b: 2]",
        "!!pairs [{a: 1}, {a: 2}]",
        "!!pairs\n- ? [a, b]\n  : value\n",
        "! {a: ! value}",
        "!!int {=: '42'}",
        "!!int '١٢'",
        "!!float '١.٢'",
        "!!binary 'SGVs bG8=!!ignored-after-padding'",
        "%TAG !e! tag:yaml.org,2002:\n---\na: !e!int '01'\n",
        "!<tag:yaml.org,2002:str> 01",
    ] {
        good(text);
    }
    for text in [
        "!!map []",
        "!!seq {}",
        "!!set []",
        "!!omap {}",
        "!!pairs {}",
        "!!omap [{a: 1, b: 2}]",
        "!!pairs [scalar]",
        "!!int nope",
        "!!float nope",
        "!!bool maybe",
        "!!binary 'a'",
        "!!binary '雪'",
        "!!str [a]",
        "!!timestamp {=: '2026-11-01'}",
        "!!python/object:danger {}",
        "!unknown scalar",
        "!!undefined value",
    ] {
        bad(text);
    }
}
#[test]
fn merges_aliases_and_unhashable_mapping_keys_fail_explicitly() {
    for text in [
        "a: *missing",
        "a: &x 1\nb: &x 2\n",
        "a: *later\nb: &later 1\n",
        "<<: scalar",
        "<<: [1]",
        "a: &a scalar\nb: {<<: *a}\n",
        "? [a,b]\n: value\n",
        "? {a: b}\n: value\n",
        "a: &a []\n? *a\n: value\n",
        "!!set {? [a,b]: null}",
        "!!merge '<<'",
        "!!value '='",
        "---\na: 1\n---\nb: 2\n",
    ] {
        bad(text);
    }
}
#[test]
fn timestamps_use_safe_constructor_calendar_and_timezone_rules() {
    for text in [
        "date: 2024-02-29",
        "date: 2000-02-29",
        "date: 2026-11-01",
        "stamp: 2026-1-2t3:04:05.123456789Z",
        "stamp: 2026-01-02 03:04:05 -05:30",
        "stamp: 2026-01-02T03:04:05+1",
        "!!timestamp '2026-1-2'",
        // Python datetime.timedelta normalizes offset minutes, then checks <24h.
        "!!timestamp '2026-1-2T3:04:05+00:90'",
        "!!timestamp |\n  2026-11-01\n",
    ] {
        good(text);
    }
    for text in [
        "date: 2023-02-29",
        "date: 1900-02-29",
        "date: 0000-01-01",
        "date: 2026-13-01",
        "date: 2026-04-31",
        "stamp: 2026-01-02T24:04:05Z",
        "stamp: 2026-01-02T03:60:05Z",
        "stamp: 2026-01-02T03:04:60Z",
        "stamp: 2026-01-02T03:04:05+24",
        "!!timestamp '2026-1-2T3:04:05+0:0:0:0'",
        "!!timestamp 'not a timestamp'",
    ] {
        bad(text);
    }
    // These are plain strings: they do not match the implicit timestamp resolver.
    good("value: 2026-1-2\nother: 2026-11-01-latest\n");
}
#[test]
fn small_graph_limits_reject_nodes_depth_scalars_and_alias_work() {
    let small = Limits {
        nodes: 8,
        depth: 2,
        scalar: 8,
        ..LIMITS
    };
    for text in ["[0,1,2,3,4,5,6,7]", "[[[0]]]", "value: '123456789'"] {
        assert!(validate(text, small).is_err(), "{text}");
    }
    let aliases = "a: &a x\nb: [*a,*a,*a,*a,*a,*a,*a,*a]\n";
    assert!(validate(aliases, small).is_err());
}
#[test]
fn shared_scalar_wrapper_scans_cannot_amplify_without_limit() {
    let mut text = "anchor: &a !!str {".to_string();
    for i in 0..200 {
        text.push_str(&format!("k{i}: ignored, "));
    }
    text.push_str("=: result}\n");
    for i in 0..100 {
        text.push_str(&format!("v{i}: !!str {{=: *a}}\n"));
    }
    let limits = Limits {
        nodes: 1024,
        ..LIMITS
    };
    assert!(Graph::parse(&text, limits).is_ok(), "raw graph is admitted");
    assert!(validate(&text, limits).unwrap_err().contains("work limit"));
}
