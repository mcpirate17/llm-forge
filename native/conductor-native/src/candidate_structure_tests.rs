use super::*;
use serde_json::{json, to_value};

#[test]
fn structure_locations_use_universal_python_newlines() {
    let source = "import os\r\ndef f():\r    handle = os.open('a', 0)\r    handle.close()\r";
    let result = facts(source, true);
    assert_eq!(result["leaks"][0]["line"], 3);
}

fn facts(source: &str, changed: bool) -> serde_json::Value {
    let mut value = to_value(document_facts(source, changed)).unwrap();
    // Keep the pre-existing display/legacy parity assertions unchanged;
    // new tests below assert canonical fields through the public API.
    for row in value["lock_orders"].as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("outer_key");
        row.as_object_mut().unwrap().remove("inner_key");
    }
    for row in value["configs"].as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("default_key");
    }
    value
}

#[test]
fn unguarded_resource_release_is_reported_but_finally_and_with_are_not() {
    let leaking = facts(
            "import os\n\ndef read_locked(path):\n    handle = os.open(path)\n    payload = handle.read()\n    handle.close()\n    return payload\n",
            true,
        );
    assert_eq!(leaking["leaks"].as_array().unwrap().len(), 1);
    assert_eq!(leaking["leaks"][0]["function"], "read_locked");
    assert_eq!(leaking["leaks"][0]["resource"], "handle");
    assert_eq!(leaking["leaks"][0]["line"], 4);
    assert_eq!(leaking["leaks"][0]["owned"], true);

    for safe in [
            "import os\n\ndef read_locked(path):\n    handle = os.open(path)\n    try:\n        return handle.read()\n    finally:\n        handle.close()\n",
            "import os\n\ndef read_locked(path):\n    with os.open(path) as handle:\n        return handle.read()\n",
            "import os\n\ndef acquire(path):\n    handle = os.open(path)\n    return handle\n",
        ] {
            assert_eq!(facts(safe, true)["leaks"], json!([]), "{safe}");
        }
}

#[test]
fn connection_close_has_distinct_ownership_fact() {
    let source = "import sqlite3\n\ndef query(path):\n    conn = sqlite3.connect(path)\n    rows = conn.execute('select 1').fetchall()\n    conn.close()\n    return rows\n";
    let result = facts(source, true);
    assert_eq!(result["leaks"].as_array().unwrap().len(), 1);
    assert_eq!(result["leaks"][0]["acquire"], "connect");
    assert_eq!(result["leaks"][0]["release"], "close");
    assert_eq!(result["leaks"][0]["owned"], false);
}

#[test]
fn lock_order_facts_anchor_the_inner_acquisition() {
    let forward = facts(
            "import threading\nalpha_lock = threading.Lock()\nbeta_lock = threading.Lock()\ndef forward():\n    with alpha_lock:\n        with beta_lock:\n            return 1\n",
            true,
        );
    let backward = facts(
        "def backward():\n    with beta_lock:\n        with alpha_lock:\n            return 2\n",
        false,
    );
    assert_eq!(
        forward["lock_orders"],
        json!([{
            "outer": "alpha_lock", "inner": "beta_lock", "line": 6
        }])
    );
    assert_eq!(
        backward["lock_orders"],
        json!([{
            "outer": "beta_lock", "inner": "alpha_lock", "line": 3
        }])
    );
}

#[test]
fn nominal_abstraction_and_subclass_facts_exclude_protocol() {
    let nominal = facts(
            "from abc import ABC, abstractmethod\nclass Storage(ABC):\n    @abstractmethod\n    def load(self, key): ...\n    @abstractmethod\n    def store(self, key, value): ...\nclass Backend(Storage):\n    def load(self, key): return None\n    def store(self, key, value): return None\n",
            true,
        );
    assert_eq!(nominal["abstractions"][0]["name"], "Storage");
    assert_eq!(nominal["abstractions"][0]["methods"], 2);
    assert!(nominal["subclasses"].as_array().unwrap().contains(&json!({
        "base": "Storage",
        "implementation": "pkg/example.py::Backend",
    })));

    let protocol = facts(
            "from typing import Protocol\nfrom abc import abstractmethod\nclass Storage(Protocol):\n    @abstractmethod\n    def load(self, key): ...\n    @abstractmethod\n    def store(self, key, value): ...\n",
            true,
        );
    assert_eq!(protocol["abstractions"], json!([]));
}

#[test]
fn configuration_facts_preserve_key_default_and_line() {
    let result = facts(
            "import os\nfrom pkg.constants import DEFAULT\nhost = os.getenv('SERVICE_HOST', DEFAULT)\nurl = os.environ.get('SERVICE_URL', 'http://localhost')\nignored = os.getenv('NO_DEFAULT')\n",
            false,
        );
    assert_eq!(
        result["configs"],
        json!([
            {"key": "SERVICE_HOST", "line": 3, "default": "DEFAULT"},
            {"key": "SERVICE_URL", "line": 4, "default": "'http://localhost'"},
        ])
    );
}

#[test]
fn unbounded_cache_requires_growth_inside_changed_function_without_eviction() {
    let growing = "_CACHE = {}\n\ndef lookup(key):\n    if key not in _CACHE:\n        _CACHE[key] = compute(key)\n    return _CACHE[key]\n";
    let result = facts(growing, true);
    assert_eq!(
        result["unbounded"],
        json!([{
            "name": "_CACHE", "defined_line": 1, "growth_line": 5
        }])
    );
    assert_eq!(facts(growing, false)["unbounded"], json!([]));

    for bounded in [
            "_CACHE = {}\ndef lookup(key):\n    if len(_CACHE) > 128:\n        _CACHE.clear()\n    _CACHE[key] = compute(key)\n",
            "import collections\n_RECENT = collections.deque(maxlen=64)\ndef record(event):\n    _RECENT.append(event)\n",
            "_REGISTRY = {}\n_REGISTRY['one'] = 1\ndef lookup(key):\n    return _REGISTRY[key]\n",
        ] {
            assert_eq!(facts(bounded, true)["unbounded"], json!([]), "{bounded}");
        }
}

#[test]
fn parser_rejects_invalid_source_without_emitting_partial_facts() {
    assert!(
        evaluate(&json!({"files":[{"path":"broken.py","text":"def f(:\n"}],
        "changed":["broken.py"]}))
        .is_err()
    );
}

fn document_facts(source: &str, changed: bool) -> FileFacts {
    let document = Document::parse(source).unwrap();
    analyze_document("pkg/example.py".into(), &document, changed).unwrap()
}
#[test]
fn bound_document_keeps_canonical_work_private_and_changed_scope_explicit() {
    let source =
        "import os\n_CACHE = {}\ndef f():\n    _CACHE['x'] = 1\nvalue = os.getenv('K', '雪')\n";
    let document = Document::parse(source).unwrap();
    let native = analyze_document("pkg/example.py".into(), &document, true).unwrap();
    assert_eq!(native.path, "pkg/example.py");
    assert_eq!(native.unbounded.len(), 1);
    assert_eq!(native.configs.len(), 1);
    assert!(native.canonical_bytes > 0);
    assert!(to_value(native).unwrap().get("canonical_bytes").is_none());
    assert!(document_facts(source, false).unbounded.is_empty());
}
#[test]
fn expression_keys_ignore_location_quote_style_parentheses_and_display_whitespace() {
    let first = document_facts("import os\nx = os.getenv('K', ('雪'))\n", false);
    let second = document_facts(
        "# location shift\n\nimport os\nx=os.getenv(\"K\", \"雪\")\n",
        false,
    );
    let other = document_facts("import os\nx = os.getenv('K', 'other')\n", false);
    assert_ne!(first.configs[0].default, second.configs[0].default);
    assert_eq!(first.configs[0].default_key, second.configs[0].default_key);
    assert_ne!(first.configs[0].default_key, other.configs[0].default_key);
    assert_eq!(first.configs[0].default_key.len(), 64);
    let lock = document_facts(
        "def f():\n    with get_lock('a'):\n        with get_lock(\"a\"):\n            pass\n",
        false,
    );
    assert!(
        lock.lock_orders.is_empty(),
        "same lock AST despite display spelling"
    );
    let changed = document_facts(
        "def f():\n    with get_lock('a'):\n        with get_lock('b'):\n            pass\n",
        false,
    );
    assert_eq!(changed.lock_orders.len(), 1);
    assert_ne!(
        changed.lock_orders[0].outer_key,
        changed.lock_orders[0].inner_key
    );
    assert_eq!(changed.lock_orders[0].outer, "get_lock('a')");
}
#[test]
fn streamed_key_matches_location_free_debug_sha_without_retaining_debug_text() {
    use ruff_python_ast::comparable::ComparableExpr;
    use sha2::{Digest, Sha256};
    let document = Document::parse("value = {'雪': [1, True, None]}\n").unwrap();
    let ast::Stmt::Assign(assign) = &document.module().body[0] else {
        panic!("assignment");
    };
    let expression = assign.value.as_ref();
    let debug = format!("{:?}", ComparableExpr::from(expression));
    let expected = format!("{:x}", Sha256::digest(debug.as_bytes()));
    let mut budget = Budget::default();
    assert_eq!(budget.key(expression).unwrap(), expected);
    assert_eq!(budget.bytes, debug.len());
    // Exact byte admission succeeds; the next nonempty formatting write fails.
    budget.bytes = 32 * 1024 * 1024 - debug.len();
    assert_eq!(budget.key(expression).unwrap(), expected);
    assert_eq!(budget.bytes, 32 * 1024 * 1024);
    assert!(budget.key(expression).unwrap_err().contains("32 MiB"));
}
#[test]
fn native_breadth_first_growth_lines_preserve_python_ast_field_order_ties() {
    let cases = [
        ("_CACHE={}\ndef f():\n    if True:\n        if True:\n            _CACHE['deep']=1\n    _CACHE.append(2)\n", 6),
        ("_CACHE={}\nclass C:\n    def method(self):\n        _CACHE['early']=1\ndef top():\n    _CACHE.append(2)\n", 6),
        ("_CACHE={}\n@decorate(_CACHE.append(1))\ndef f(x=_CACHE.append(2)):\n    _CACHE['body']=3\n", 4),
        ("_CACHE={}\ndef f():\n    return (\n        _CACHE.append(1)\n        if _CACHE.append(2)\n        else _CACHE.append(3)\n    )\n", 5),
        ("_CACHE={}\ndef f():\n    return {\n        'a': _CACHE.append(1),\n        _CACHE.append(2): 0,\n    }\n", 5),
        ("_CACHE={}\ndef f():\n    if True:\n        pass\n    else:\n        _CACHE['else']=1\n    _CACHE.append(2)\n", 6),
        ("_CACHE={}\ndef f():\n    if True:\n        pass\n    elif False:\n        _CACHE['elif']=1\n    _CACHE.append(2)\n", 7),
    ];
    for (source, expected) in cases {
        let facts = document_facts(source, true);
        assert_eq!(facts.unbounded.len(), 1, "{source}");
        assert_eq!(facts.unbounded[0].growth_line, expected, "{source}");
    }
}
#[test]
fn shared_fact_limit_is_error_and_batch_does_not_drop_failed_file() {
    let mut source = "def f():\n    with alpha_lock:\n        with beta_lock:\n            with gamma_lock:\n                pass\n".to_owned();
    for _ in 0..4095 {
        source.push_str("os.getenv('K', 1)\n");
    }
    let document = Document::parse(&source).unwrap();
    assert!(analyze_document("a.py".into(), &document, false)
        .unwrap_err()
        .contains("4096"));
    assert!(
        evaluate(&json!({"files":[{"path":"a.py","text":source}], "changed":[]}))
            .unwrap_err()
            .contains("4096")
    );
}
#[test]
fn nested_default_formatting_amplification_fails_after_document_admission() {
    let mut source = "value = ".to_owned();
    for _ in 0..48 {
        source.push_str("os.getenv('K', ");
    }
    source.push('\'');
    source.push_str(&"x".repeat(900_000));
    source.push('\'');
    source.push_str(&")".repeat(48));
    source.push('\n');
    let document = Document::parse(&source).unwrap();
    let error = analyze_document("a.py".into(), &document, false).unwrap_err();
    assert!(error.contains("formatting") && error.contains("32 MiB"));
    assert!(
        evaluate(&json!({"files":[{"path":"a.py","text":source}], "changed":[]}))
            .unwrap_err()
            .contains("formatting")
    );
}
#[test]
fn native_document_api_rejects_invalid_path_and_batch_requires_valid_peers() {
    let document = Document::parse("x = 1\n").unwrap();
    for path in ["", "../x.py", "/x.py", "a//x.py", "x\n.py"] {
        assert!(analyze_document(path.into(), &document, false).is_err());
    }
    assert!(
        evaluate(&json!({"files":[{"path":"broken.py","text":"def f(:"},
        {"path":"valid.py","text":"x=1"}], "changed":["valid.py"]}))
        .is_err()
    );
    assert!(Document::parse(&"x".repeat(1024 * 1024 + 1))
        .err()
        .unwrap()
        .to_string()
        .contains("1 MiB"));
}
#[test]
fn repeated_nested_finally_analysis_has_finite_work_after_ast_admission() {
    let mut source = String::new();
    for block in 0..100 {
        for depth in 0..35 {
            source.push_str(&format!("{}def f_{block}_{depth}():\n", " ".repeat(depth)));
        }
        for depth in 35..59 {
            source.push_str(&format!(
                "{}try:\n{}pass\n{}finally:\n",
                " ".repeat(depth),
                " ".repeat(depth + 1),
                " ".repeat(depth)
            ));
        }
        source.push_str(&format!("{}pass\n", " ".repeat(59)));
    }
    let document = Document::parse(&source).unwrap();
    assert!(document.nodes() < crate::candidate_python::MAX_NODES);
    assert!(analyze_document("nested.py".into(), &document, true)
        .unwrap_err()
        .contains("work limit"));
}
