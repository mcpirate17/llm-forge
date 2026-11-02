//! Observable candidate rules formerly exercised through CPython compatibility helpers.
#![cfg(feature = "source-analysis")]
use conductor_native::candidate_checks::evaluate;
use serde_json::{json, Value};

fn analyze(source: &str, hot: bool) -> Value {
    evaluate(
        "python-ast",
        &json!({"path":"lane.py", "text":source,
        "changed_lines":[], "classes":["python"], "hot":hot}),
    )
    .unwrap()
}
fn matching<'a>(result: &'a Value, rule: &str) -> Vec<&'a Value> {
    result["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["rule_id"] == rule)
        .collect()
}

#[test]
fn dynamic_execution_and_deserialization_resolve_only_named_callees() {
    for (source, rule) in [
        ("eval(payload)", Some("dynamic-execution")),
        ("exec(payload)", Some("dynamic-execution")),
        ("os.system(cmd)", Some("dynamic-execution")),
        ("pickle.load(handle)", Some("unsafe-deserialization")),
        ("yaml.load(stream)", Some("unsafe-yaml")),
        ("obj.eval()", None),
        ("self.eval()", None),
        ("torch.nn.Module.eval(model)", None),
        ("model.to(device).eval()", None),
        ("build().exec()", None),
        ("registry['key'].eval()", None),
        ("(a + b).eval()", None),
    ] {
        let result = analyze(source, false);
        let findings = result["findings"].as_array().unwrap();
        if let Some(rule) = rule {
            assert_eq!(matching(&result, rule).len(), 1, "{source}: {result}");
            assert_eq!(findings.len(), 1, "{source}: {result}");
        } else {
            assert!(findings.is_empty(), "{source}: {result}");
        }
    }
}

#[test]
fn dynamic_function_calls_keep_messages_and_source_locations() {
    let source = "import os\ndef check_host(value):\n    return os.system(value)\ndef refresh(model):\n    return model.eval()\nmake().eval()\neval_used = eval('1')\nexec_used = exec('pass')\n";
    let result = analyze(source, false);
    let findings = matching(&result, "dynamic-execution");
    assert_eq!(
        findings
            .iter()
            .map(|f| f["line"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [3, 7, 8]
    );
    assert_eq!(
        findings
            .iter()
            .map(|f| f["message"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "unsafe dynamic execution via os.system",
            "unsafe dynamic execution via eval",
            "unsafe dynamic execution via exec"
        ]
    );
}

#[test]
fn protocol_ellipsis_exempts_only_the_protocol_class_scope() {
    let source = concat!(
        "from typing import Protocol\nclass Sink(Protocol):\n",
        "    def put(self, key: str, value: int) -> None:\n        ...\n",
        "    @property\n    def size(self) -> int:\n        ...\n",
        "class Nested(Protocol):\n    class Inner(Protocol):\n",
        "        def deep(self) -> str:\n            ...\n",
        "    def flat(self) -> None:\n        ...\n",
        "    class Concrete:\n        def inner_stub(self): ...\n",
        "class Impl:\n    def real(self):\n        return 1\n    def missing(self): ...\n"
    );
    let expected = ["def inner_stub", "def missing"].map(|marker| {
        source[..source.find(marker).unwrap()]
            .bytes()
            .filter(|b| *b == b'\n')
            .count() as u64
            + 1
    });
    let result = analyze(source, false);
    let lines: Vec<_> = matching(&result, "ellipsis-stub")
        .iter()
        .map(|f| f["line"].as_u64().unwrap())
        .collect();
    assert_eq!(lines, expected);
}

const KERNEL: &str = "import triton\nimport triton.language as tl\n@triton.jit\ndef kernel(out_ptr, S: tl.constexpr):\n    for scale in tl.static_range(S):\n        for offset in tl.static_range(8):\n            tl.store(out_ptr + scale * 8 + offset, 0.0)\n";

#[test]
fn native_kernel_decorators_and_static_ranges_do_not_trigger_python_hot_loop_rule() {
    for decorator in [
        "@jit",
        "@numba.cuda.jit",
        "@numba.njit",
        "@numba.jit",
        "@triton.jit",
        "@triton.autotune(configs=[], key=['S'])",
        "@triton.heuristics({})",
    ] {
        let source = format!("{decorator}\ndef kernel(values):\n    for row in values:\n        for column in row:\n            store(row, column)\n");
        assert!(
            matching(&analyze(&source, true), "nested-loop-hotpath").is_empty(),
            "{decorator}"
        );
    }
    assert!(matching(&analyze(KERNEL, true), "nested-loop-hotpath").is_empty());
}

#[test]
fn kernel_scope_does_not_suppress_following_python_functions_or_hot_path_selection() {
    for body in ["total += value", "print(value)"] {
        let source = format!("{KERNEL}\ndef plain(rows):\n    total = 0\n    for row in rows:\n        for value in row:\n            {body}\n    return total\n");
        let result = analyze(&source, true);
        let findings = matching(&result, "nested-loop-hotpath");
        assert_eq!(findings.len(), 1);
        let line = findings[0]["line"].as_u64().unwrap() as usize;
        assert_eq!(
            source.lines().nth(line - 1).unwrap().trim(),
            "for value in row:"
        );
        assert!(matching(&analyze(&source, false), "nested-loop-hotpath").is_empty());
    }
}

#[test]
fn malformed_change_admission_and_finding_exhaustion_are_errors() {
    for changed in [
        json!([0]),
        json!([-1]),
        json!(["1"]),
        json!([2]),
        json!([null]),
    ] {
        assert!(evaluate(
            "python-ast",
            &json!({"path":"a.py", "text":"x=1\n",
            "changed_lines":changed, "classes":[], "hot":false})
        )
        .is_err());
    }
    let source = "eval('x')\n".repeat(4097);
    let error = evaluate(
        "python-ast",
        &json!({"path":"a.py", "text":source,
        "changed_lines":[], "classes":[], "hot":false}),
    )
    .unwrap_err();
    assert!(error.contains("findings exceed 4096"), "{error}");
}

#[test]
fn adversarial_novel_source_keeps_all_rule_severities() {
    let source = concat!(
        "# TODO remove scaffold\nimport json, pickle, subprocess, yaml\nimport random\n",
        "def pending(): pass\ndef missing(): ...\ndef unsafe(items):\n",
        "    for outer in items:\n        for inner in items:\n            json.loads('{}')\n",
        "    eval('1')\n    pickle.loads(b'x')\n    yaml.load('x')\n",
        "    subprocess.run('x', shell=True)\n    raise NotImplementedError()\n",
        "RANDOM = random.random()\ndef softmax_fallback(): return 'attention fallback'\n"
    );
    let result = evaluate(
        "python-ast",
        &json!({"path":"novel/mechanism.py", "text":source,
        "changed_lines":(1..=source.lines().count()).collect::<Vec<_>>(),
        "classes":["python","novel"], "hot":true}),
    )
    .unwrap();
    for (rule, severity) in [
        ("pass-stub", "high"),
        ("ellipsis-stub", "high"),
        ("dynamic-execution", "critical"),
        ("unsafe-deserialization", "critical"),
        ("unsafe-yaml", "critical"),
        ("unsafe-shell", "high"),
        ("not-implemented-stub", "high"),
        ("softmax-shaped-fallback", "critical"),
        ("partial-implementation-marker", "high"),
        ("nondeterministic-research", "high"),
        ("repeated-parsing-io", "high"),
        ("nested-loop-hotpath", "medium"),
    ] {
        let findings = matching(&result, rule);
        assert!(!findings.is_empty(), "{rule}: {result}");
        assert!(findings
            .iter()
            .all(|finding| finding["severity"] == severity));
    }
}

#[test]
fn promotion_findings_require_changed_stage1_and_preserve_metric_contract() {
    let inspect = |path: &str, source: &str, changed: &[usize]| {
        evaluate(
            "python-ast",
            &json!({"path":path, "text":source, "changed_lines":changed,
                "classes":["python"], "hot":false}),
        )
        .unwrap()
    };
    let path = "research/tools/result_record_probe.py";
    let source = "stage1_passed = True\nscore = 1\ndtype = 'bad'\n";
    let result = inspect(path, source, &[1, 2, 3]);
    let findings = matching(&result, "partial-promotion-write");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["severity"], "critical");
    assert_eq!(findings[0]["path"], path);
    for result in [
        inspect(path, source, &[2, 3]),
        inspect("unrelated.py", source, &[1]),
        inspect(
            path,
            "stage1_passed = True\nmetrics = ('wikitext', 'hellaswag', 'blimp', 'binding')\n",
            &[1],
        ),
    ] {
        assert!(matching(&result, "partial-promotion-write").is_empty());
    }
    let incomplete = inspect(
        path,
        "stage1_passed = True\nmetrics = ('wikitext', 'hellaswag', 'binding')\n",
        &[1],
    );
    assert_eq!(matching(&incomplete, "partial-promotion-write").len(), 1);
}
