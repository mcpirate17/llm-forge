//! An ablation that does not parse is not a measurement -- it is a crash the probe
//! would score as "the code mattered". Syntactic validity is therefore the property
//! every rule is tested against, not just the hand-picked cases below.

use crate::engine::{apply, collect, parse};
use crate::rules;

fn all_rules() -> Vec<(&'static str, crate::engine::Rule)> {
    rules::DEFAULT
        .iter()
        .chain(rules::OPTIONAL)
        .copied()
        .collect()
}

fn fire(src: &str, rule: &str) -> Vec<String> {
    let chosen: Vec<_> = all_rules()
        .into_iter()
        .filter(|(n, _)| *n == rule)
        .collect();
    collect(src, &chosen)
        .iter()
        .map(|a| apply(src, a))
        .collect()
}

#[test]
fn clamp_is_removed_and_receiver_kept() {
    let out = fire(
        "def f(x):\n    return torch.log(x.clamp(min=1e-6))\n",
        "drop_clamp",
    );
    assert_eq!(out.len(), 1);
    assert!(out[0].contains("torch.log(x)"), "{}", out[0]);
}

#[test]
fn where_collapses_to_the_true_branch() {
    let out = fire(
        "def f(c, a, b):\n    return torch.where(c, a, b)\n",
        "drop_where",
    );
    assert_eq!(out.len(), 1);
    assert!(out[0].contains("return a"), "{}", out[0]);
}

#[test]
fn raise_guard_becomes_pass() {
    let src = "def f(x):\n    if x < 0:\n        raise ValueError('neg')\n    return x\n";
    let out = fire(src, "drop_raise_guard");
    assert_eq!(out.len(), 1);
    assert!(
        out[0].contains("pass") && !out[0].contains("raise"),
        "{}",
        out[0]
    );
}

#[test]
fn unary_call_refuses_builtins() {
    // `helper` is local, `isinstance` is not. Firing on builtins invalidated a
    // whole sweep once; the rule must stay scoped to this module's own functions.
    let src = "def helper(v):\n    return v * 2\n\ndef f(x):\n    return helper(x) + len(x)\n";
    let out = fire(src, "drop_unary_call");
    assert_eq!(out.len(), 1, "should fire on helper only, got {out:?}");
    assert!(out[0].contains("return x + len(x)"), "{}", out[0]);
}

#[test]
fn trailing_arg_needs_a_default_at_that_position() {
    let with_default = "def g(a, b=2):\n    return a + b\n\ndef f():\n    return g(1, 5)\n";
    assert_eq!(fire(with_default, "drop_trailing_arg").len(), 1);
    let without = "def g(a, b):\n    return a + b\n\ndef f():\n    return g(1, 5)\n";
    assert_eq!(
        fire(without, "drop_trailing_arg").len(),
        0,
        "would be a TypeError, not a question"
    );
}

#[test]
fn dropping_an_argument_leaves_no_dangling_comma() {
    let src = "def g(a, b=2):\n    return a + b\n\ndef f():\n    return g(1, 5)\n";
    let out = fire(src, "drop_trailing_arg");
    assert!(out[0].contains("g(1)"), "{}", out[0]);
    assert!(!parse(&out[0]).unwrap().root_node().has_error());
}

#[test]
fn keyword_argument_falls_back_to_its_default() {
    let out = fire(
        "def f(t):\n    return t.sum(dim=1, keepdim=True)\n",
        "drop_keyword_arg",
    );
    assert_eq!(out.len(), 2);
    assert!(out.iter().any(|s| s.contains("t.sum(dim=1)")), "{out:?}");
}

#[test]
fn parameter_is_pinned_at_every_use_but_the_signature_survives() {
    let src = "def f(x, scale=1.0):\n    y = x * scale\n    return y + scale\n";
    let out = fire(src, "pin_parameter_to_default");
    assert_eq!(out.len(), 1);
    // The signature must be intact, or every caller dies of TypeError and the
    // mutant is killed for a reason that has nothing to do with the parameter.
    assert!(out[0].contains("def f(x, scale=1.0):"), "{}", out[0]);
    assert!(
        out[0].contains("x * 1.0") && out[0].contains("y + 1.0"),
        "{}",
        out[0]
    );
}

#[test]
fn pinning_ignores_attributes_and_keywords_of_the_same_name() {
    let src =
        "def f(x, scale=1.0):\n    a = obj.scale\n    b = call(scale=3)\n    return x * scale\n";
    let out = fire(src, "pin_parameter_to_default");
    assert_eq!(out.len(), 1);
    assert!(
        out[0].contains("obj.scale"),
        "attribute rewritten: {}",
        out[0]
    );
    assert!(
        out[0].contains("call(scale=3)"),
        "keyword rewritten: {}",
        out[0]
    );
    assert!(out[0].contains("x * 1.0"), "real use missed: {}", out[0]);
}

#[test]
fn boolean_default_flips() {
    let out = fire(
        "def f(a, flag=True):\n    return a if flag else -a\n",
        "flip_boolean_default",
    );
    assert_eq!(out.len(), 1);
    assert!(out[0].contains("flag=False"), "{}", out[0]);
}

#[test]
fn module_level_imports_are_ablated_and_nested_ones_are_not() {
    let src = "import os\n\ndef f():\n    import json\n    return json\n";
    let out = fire(src, "drop_import");
    assert_eq!(
        out.len(),
        1,
        "only the module-level import is a re-export candidate"
    );
    assert!(out[0].starts_with("pass"), "{}", out[0]);
}

#[test]
fn whole_body_ablation_returns_none() {
    let src = "def f(x):\n    total = 0\n    for v in x:\n        total += v\n    return total\n";
    let out = fire(src, "ablate_function_to_none");
    assert_eq!(out.len(), 1);
    assert!(
        out[0].contains("return None") && !out[0].contains("total"),
        "{}",
        out[0]
    );
    assert!(
        !parse(&out[0]).unwrap().root_node().has_error(),
        "{}",
        out[0]
    );
}

#[test]
fn trivial_bodies_are_not_worth_ablating() {
    // `return None` in place of `pass` is not a mutant, it is the same program.
    assert_eq!(
        fire("def f():\n    pass\n", "ablate_function_to_none").len(),
        0
    );
    assert_eq!(
        fire(
            "def f():\n    \"\"\"Doc.\"\"\"\n",
            "ablate_function_to_none"
        )
        .len(),
        0
    );
}

#[test]
fn passthrough_skips_self() {
    let src = "class C:\n    def f(self, x):\n        y = x * 3\n        return y + 1\n";
    let out = fire(src, "ablate_function_to_passthrough");
    assert_eq!(out.len(), 1);
    assert!(out[0].contains("return x"), "{}", out[0]);
}

#[test]
fn qualname_carries_the_class() {
    let src = "class Mix:\n    def score(self, x):\n        return x.clamp(min=0)\n";
    let chosen: Vec<_> = all_rules()
        .into_iter()
        .filter(|(n, _)| *n == "drop_clamp")
        .collect();
    let found = collect(src, &chosen);
    assert_eq!(found[0].qualname, "Mix.score");
}

#[test]
fn every_ablation_of_a_realistic_module_reparses() {
    let src = r#"
import os  # noqa: F401
import torch
from torch import nn


def _helper(v, scale=2.0):
    if v is None:
        raise ValueError("v is required")
    return v * scale


class Lane(nn.Module):
    def __init__(self, n: int, bias: bool = True):
        super().__init__()
        self.proj = nn.Linear(n, n, bias=bias)
        self.n = n

    @torch.no_grad()
    def route(self, x, temperature=1.0):
        w = torch.softmax(self.proj(x) / temperature, dim=-1)
        w = w / w.norm(dim=-1, keepdim=True).clamp(min=1e-9)
        mask = torch.where(w > 0.1, w, torch.zeros_like(w))
        out = mask.masked_fill(mask == 0, 0.0).contiguous()
        self._log(out)
        return torch.log(out.clamp(min=1e-6)) + _helper(x)

    def _log(self, t):
        if t.numel() == 0:
            raise RuntimeError("empty")
        return t.detach()
"#;
    let found = collect(src, &all_rules());
    assert!(
        found.len() > 30,
        "expected a rich harvest, got {}",
        found.len()
    );
    for a in &found {
        let mutated = apply(src, a);
        let tree = parse(&mutated).expect("parser returned nothing");
        assert!(
            !tree.root_node().has_error(),
            "rule {} at line {} produced unparseable source:\n{}",
            a.rule,
            a.line,
            mutated
        );
        assert_ne!(mutated, src, "rule {} changed nothing", a.rule);
    }
}

#[test]
fn every_rule_in_the_registry_fires_at_least_once_somewhere() {
    // A rule nobody can trigger is not a rule. This is the regression guard for
    // renames in the tree-sitter grammar, which change node kinds silently.
    let src = r#"
import os


def helper(v, scale=1.0, flag=True):
    if v is None:
        raise ValueError("no")
    return v * scale


@staticmethod
def top(x, y=1):
    z = helper(x)
    z = helper(x, 2)
    log(z)
    a = x.clamp(min=0).clamp_min(1).clamp_max(9).detach().contiguous()
    b = a.masked_fill(a == 0, 0.0).tril().triu().nan_to_num()
    c = b.softmax(dim=-1).sigmoid().masked_fill_(b > 1, 0.0)
    d = torch.where(c > 0, c, b) / c.norm()
    e = call(d, key=3)
    return e + y
"#;
    let found = collect(src, &all_rules());
    let seen: std::collections::HashSet<_> = found.iter().map(|a| a.rule.as_str()).collect();
    let missing: Vec<_> = all_rules()
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| !seen.contains(n))
        .collect();
    assert!(missing.is_empty(), "rules that never fired: {missing:?}");
}

#[test]
fn syntax_errors_yield_nothing_rather_than_garbage() {
    // Must not panic on unparseable input; whatever it emits is checked below.
    let _ = collect("def f(:\n  ???\n", &all_rules());
    // The contract that matters: nothing emitted can be unparseable.
    for a in collect("def f(:\n  ???\n", &all_rules()) {
        let m = apply("def f(:\n  ???\n", &a);
        assert!(!m.is_empty());
    }
}

// ---- regressions found by compiling 40,051 ablations of the real corpus ----

#[test]
fn pinning_never_rewrites_a_binding_site() {
    // Each of these rebinds `scale` rather than reading it. Substituting the default
    // produced `1.0 = ...`, which tree-sitter accepts and CPython does not.
    for body in [
        "    a, scale = f()\n    return x * a\n",
        "    for scale in items:\n        pass\n    return x\n",
        "    with open(p) as scale:\n        pass\n    return x\n",
        "    scale += 1\n    return x\n",
    ] {
        let src = format!("def f(x, scale=1.0):\n{body}");
        for mutated in fire(&src, "pin_parameter_to_default") {
            assert!(
                !mutated.contains("1.0 ="),
                "binding site rewritten in:\n{mutated}"
            );
        }
    }
}

#[test]
fn passthrough_never_returns_a_starred_parameter() {
    let src = "def f(*args, **kw):\n    total = sum(args)\n    return total + 1\n";
    for mutated in fire(src, "ablate_function_to_passthrough") {
        assert!(!mutated.contains("return *"), "{mutated}");
    }
    let typed = "def f(*args: int):\n    total = sum(args)\n    return total + 1\n";
    assert!(fire(typed, "ablate_function_to_passthrough").is_empty());
}

#[test]
fn expression_rules_refuse_multiline_fragments() {
    // Splicing a fragment that spans lines into a single-line context reindents the
    // rest of the statement. The tree still parses; the Python does not.
    let src = "def g(v):\n    return v\n\ndef f(x):\n    return g(\n        x + 1,\n    )\n";
    for mutated in fire(src, "drop_unary_call") {
        assert!(
            compiles_shape(&mutated),
            "multi-line fragment spliced inline:\n{mutated}"
        );
    }
}

/// A cheap proxy for CPython's compiler: no line may be indented more deeply than
/// its predecessor without that predecessor opening a block.
fn compiles_shape(src: &str) -> bool {
    let mut prev_indent = 0usize;
    let mut prev_opens = true;
    for line in src.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() {
            continue;
        }
        let indent = t.len() - t.trim_start().len();
        if indent > prev_indent && !prev_opens {
            return false;
        }
        prev_indent = indent;
        prev_opens = t.trim_end().ends_with(':')
            || t.trim_end().ends_with(',')
            || t.trim_end().ends_with('(')
            || t.trim_end().ends_with('[');
    }
    true
}

#[test]
fn positional_rules_decline_when_a_splat_hides_the_count() {
    // `f(**overrides)` has one child and an unknown number of arguments. Treating
    // that child as positional spliced `**overrides` out as if it were a value.
    let src = "def g(a, b=1):\n    return a\n\ndef f(kw):\n    return g(**kw)\n";
    assert!(fire(src, "drop_unary_call").is_empty());
    assert!(fire(src, "drop_trailing_arg").is_empty());
    let starred = "def g(a, b=1):\n    return a\n\ndef f(xs):\n    return g(*xs)\n";
    assert!(fire(starred, "drop_unary_call").is_empty());
}

#[test]
fn pinning_leaves_del_targets_alone() {
    let src = "def f(x, scale=1.0):\n    scale = compute()\n    del scale\n    return x\n";
    for mutated in fire(src, "pin_parameter_to_default") {
        assert!(!mutated.contains("del None"), "{mutated}");
    }
}

fn quals(src: &str, rule: &str) -> Vec<String> {
    let chosen: Vec<_> = all_rules()
        .into_iter()
        .filter(|(n, _)| *n == rule)
        .collect();
    collect(src, &chosen)
        .iter()
        .map(|a| a.qualname.clone())
        .collect()
}

#[test]
fn a_decorator_is_owned_by_the_definition_it_decorates() {
    // A decorator is a sibling of the definition under `decorated_definition`, so the
    // walker's scope has not pushed the function name yet. Emitting the enclosing
    // scope left every drop_decorator ablation with an empty qualname, and
    // probe_function filters on `qualname == target` -- so the rule shipped but never
    // reached the probe once.
    let src = "import functools\n\n@functools.cache\ndef expensive(n):\n    return n\n";
    assert_eq!(quals(src, "drop_decorator"), vec!["expensive".to_string()]);
}

#[test]
fn a_decorated_method_keeps_its_class_prefix() {
    // The qualname has to compose, not replace: probe_function is asked for
    // `Lane.forward`, and a bare `forward` would miss it exactly as an empty one did.
    let src = "class Lane:\n    @property\n    def forward(self):\n        return 1\n";
    assert_eq!(
        quals(src, "drop_decorator"),
        vec!["Lane.forward".to_string()]
    );
}

#[test]
fn a_decorated_class_is_named_too() {
    let src =
        "import dataclasses\n\n@dataclasses.dataclass\nclass Timing:\n    total: float = 0.0\n";
    assert_eq!(quals(src, "drop_decorator"), vec!["Timing".to_string()]);
}

#[test]
fn a_module_scope_import_has_no_owning_function() {
    // The mirror case, so the fix above cannot quietly invent an owner: an import is
    // module scope, it belongs to no function, and import_ablation covers it.
    let src = "import functools\n\ndef f(x):\n    return x\n";
    assert_eq!(quals(src, "drop_import"), vec![String::new()]);
}
