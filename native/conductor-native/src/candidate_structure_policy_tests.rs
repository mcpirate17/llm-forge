use super::super::{AbstractionFact, ConfigFact, FileFacts, LockOrderFact, SubclassFact};
use super::*;

fn payload(sources: &[(&str, &str)], changed: &[&str]) -> Value {
    json!({"files":sources.iter().map(|(path,text)| json!({"path":path,"text":text}))
        .collect::<Vec<_>>(),"changed":changed})
}
fn rules(output: &Value) -> BTreeSet<&str> {
    output["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["rule_id"].as_str().unwrap())
        .collect()
}
fn empty(path: &str) -> FileFacts {
    FileFacts {
        path: path.into(),
        unbounded: vec![],
        leaks: vec![],
        lock_orders: vec![],
        abstractions: vec![],
        subclasses: vec![],
        configs: vec![],
        canonical_bytes: 0,
    }
}
fn config(key: &str, display: &str, canonical: &str, line: usize) -> ConfigFact {
    ConfigFact {
        key: key.into(),
        default: display.into(),
        default_key: canonical.into(),
        line,
    }
}
fn lock(outer: &str, inner: &str, outer_key: &str, inner_key: &str, line: usize) -> LockOrderFact {
    LockOrderFact {
        outer: outer.into(),
        inner: inner.into(),
        outer_key: outer_key.into(),
        inner_key: inner_key.into(),
        line,
    }
}

#[test]
fn lifecycle_severities_and_success_path_messages_match_legacy_policy() {
    let source = "import os\nimport sqlite3\n_CACHE = {}\ndef lookup(k):\n    _CACHE[k] = k\n    return k\ndef load(path):\n    h = os.open(path)\n    h.read()\n    h.close()\ndef query(path):\n    conn = sqlite3.connect(path)\n    conn.execute('select 1')\n    conn.close()\n";
    let output = evaluate(&payload(&[("a.py", source)], &["a.py"])).unwrap();
    let findings = output["findings"].as_array().unwrap();
    for (rule, severity) in [
        ("unbounded-module-cache", "medium"),
        ("cleanup-not-on-failure-path", "high"),
        ("connection-close-not-on-failure-path", "medium"),
    ] {
        assert_eq!(
            findings.iter().find(|f| f["rule_id"] == rule).unwrap()["severity"],
            severity
        );
    }
    let cache = findings
        .iter()
        .find(|f| f["rule_id"] == "unbounded-module-cache")
        .unwrap();
    assert_eq!(cache["line"], 5);
    assert_eq!(
        cache["evidence"],
        json!({"container":"_CACHE","defined_line":3})
    );
    assert!(cache["message"]
        .as_str()
        .unwrap()
        .contains("module-level '_CACHE' (defined line 3)"));
    assert_eq!(output["metrics"]["modules_indexed"], 1);
}

#[test]
fn finally_with_ownership_transfer_and_eviction_are_negative_controls() {
    let source = "import os\n_CACHE = {}\ndef lookup(k):\n    _CACHE.clear()\n    _CACHE[k] = k\ndef guarded(p):\n    h = os.open(p)\n    try:\n        return h.read()\n    finally:\n        h.close()\ndef context(p):\n    with os.open(p) as h:\n        return h.read()\ndef transfer(p):\n    h = os.open(p)\n    return h\n";
    assert!(rules(&evaluate(&payload(&[("a.py", source)], &["a.py"])).unwrap()).is_empty());
    let output = evaluate(&payload(&[("a.py", source)], &[])).unwrap();
    assert!(rules(&output).is_empty());
}

#[test]
fn canonical_lock_keys_match_equivalent_displays_and_only_changed_side_is_reported() {
    let mut a = empty("a.py");
    a.lock_orders
        .push(lock("(alpha_lock)", "beta_lock", "alpha", "beta", 9));
    let mut b = empty("b.py");
    b.lock_orders
        .push(lock("beta_lock", "alpha_lock", "beta", "alpha", 6));
    let output = evaluate_facts(&[a, b], &BTreeSet::from(["b.py"])).unwrap();
    assert_eq!(
        output["findings"],
        json!([{
            "check_id":"structure-audit","rule_id":"inconsistent-lock-order","severity":"high",
            "path":"b.py","line":6,
            "message":"takes beta_lock before alpha_lock, but a.py:9 takes them in the opposite order; the two orderings can deadlock",
            "help":"Impose one global lock order, or take both under a single guard.",
            "evidence":{"outer":"beta_lock","inner":"alpha_lock","opposite_sites":"a.py:9"}
        }])
    );
}

#[test]
fn abstraction_policy_accepts_zero_or_one_implementation_but_skips_protocol_and_small_interfaces() {
    let mut file = empty("a.py");
    for (name, methods) in [("Zero", 2), ("One", 3), ("Two", 2), ("Small", 1)] {
        file.abstractions.push(AbstractionFact {
            name: name.into(),
            methods,
            line: 1,
        });
    }
    for (base, implementation) in [
        ("One", "a.py:Only"),
        ("Two", "a.py:First"),
        ("Two", "a.py:Second"),
    ] {
        file.subclasses.push(SubclassFact {
            base: base.into(),
            implementation: implementation.into(),
        });
    }
    let output = evaluate_facts(&[file], &BTreeSet::from(["a.py"])).unwrap();
    let names = output["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["evidence"]["abstraction"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["One", "Zero"]);
    let source = "from typing import Protocol\nfrom abc import abstractmethod\nclass P(Protocol):\n    @abstractmethod\n    def first(self): ...\n    @abstractmethod\n    def second(self): ...\n";
    assert!(
        !rules(&evaluate(&payload(&[("p.py", source)], &["p.py"])).unwrap())
            .contains("single-implementation-abstraction")
    );
}

#[test]
fn config_conflicts_use_canonical_keys_and_keep_raw_display_schema() {
    let mut a = empty("a.py");
    a.configs.push(config("HOST", "'localhost'", "same", 3));
    let mut b = empty("b.py");
    b.configs.push(config("HOST", "\"localhost\"", "same", 4));
    let output = evaluate_facts(&[a, b], &BTreeSet::from(["a.py", "b.py"])).unwrap();
    assert!(rules(&output).is_empty());
    let mut a = empty("a.py");
    a.configs
        .push(config("HOST", "'localhost'", "localhost", 3));
    let mut b = empty("b.py");
    b.configs.push(config("HOST", "'127.0.0.1'", "loopback", 4));
    let output = evaluate_facts(&[a, b], &BTreeSet::from(["a.py"])).unwrap();
    let finding = &output["findings"][0];
    assert_eq!(
        finding["evidence"],
        json!({"key":"HOST","default":"'localhost'","conflicting":["'127.0.0.1'"]})
    );
    assert_eq!(
        finding["message"],
        "HOST defaults to 'localhost' here but to '127.0.0.1' elsewhere in the candidate tree"
    );
    let source_a = "import os\nx = os.getenv('K', (1))\n";
    let source_b = "import os\nx = os.environ.get(\"K\", 1)\n";
    assert!(!rules(
        &evaluate(&payload(
            &[("a.py", source_a), ("b.py", source_b)],
            &["a.py"]
        ))
        .unwrap()
    )
    .contains("duplicate-config-default"));
}

#[test]
fn output_sort_is_path_then_line_then_rule_and_files_are_sorted_unique() {
    let mut a = empty("a.py");
    a.configs.push(config("K", "1", "one", 5));
    a.abstractions.push(AbstractionFact {
        name: "A".into(),
        methods: 2,
        line: 5,
    });
    let mut z = empty("z.py");
    z.configs.push(config("K", "2", "two", 1));
    let output = evaluate_facts(&[a, z], &BTreeSet::from(["z.py", "a.py"])).unwrap();
    let triples = output["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap(),
                f["line"].as_u64().unwrap(),
                f["rule_id"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        triples,
        [
            ("a.py", 5, "duplicate-config-default"),
            ("a.py", 5, "single-implementation-abstraction"),
            ("z.py", 1, "duplicate-config-default")
        ]
    );
    assert_eq!(output["files"], json!(["a.py", "z.py"]));
}

#[test]
fn malformed_unchanged_peers_and_inventory_limits_fail_explicitly() {
    for input in [
        payload(
            &[("good.py", "x=1"), ("bad.py", "def broken(:\n")],
            &["good.py"],
        ),
        payload(&[("a.py", ""), ("a.py", "")], &[]),
        payload(&[("../escape.py", "")], &[]),
        payload(&[], &["missing.py"]),
        json!({"files":[{"path":"a.py","text":"x".repeat(MAX_SOURCE+1)}],"changed":[]}),
        json!({"files":vec![json!({"path":"a.py","text":""});MAX_FILES+1],"changed":[]}),
    ] {
        assert!(evaluate(&input).is_err());
    }
    let error = evaluate(&payload(&[("bad.py", "def broken(:\n")], &[])).unwrap_err();
    assert!(error.contains("incomplete"));
    assert!(error.contains("bad.py"));
}

#[test]
fn amplified_conflicting_defaults_are_reserved_before_any_finding_is_built() {
    let mut file = empty("a.py");
    for i in 0..100 {
        file.configs.push(config(
            "K",
            &format!("{i}{}", "x".repeat(4096)),
            &format!("{i}"),
            i + 1,
        ));
    }
    let files = [file];
    let index = index(&files);
    let changed = BTreeSet::from(["a.py"]);
    let mut budget = Budget::new(&changed).unwrap();
    let mut findings = Vec::new();
    assert!(configs(&index, &changed, &mut budget, &mut findings, false).is_err());
    assert!(findings.is_empty());
    assert!(evaluate_facts(&files, &changed)
        .unwrap_err()
        .contains("evidence output"));
}
