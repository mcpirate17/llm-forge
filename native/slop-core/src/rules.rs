//! One ablation rule per construct. A rule earns its place only by discriminating:
//! LIVE on code that matters, no-difference on code that does not. Rules that have
//! been measured and never discriminated live in `OPTIONAL` and are opt-in.

use crate::engine::{named, params_of, Ablation, Ctx, Edit, Rule};
use tree_sitter::Node;

/// A sub-expression substitution has to stay on one line. Splicing a fragment that
/// spans lines into the middle of a statement reindents whatever followed it, which
/// CPython rejects even though tree-sitter parses it happily.
fn inline(text: String) -> Option<String> {
    if text.contains('\n') {
        None
    } else {
        Some(text)
    }
}

fn edit(start: usize, end: usize, replacement: &str) -> Edit {
    Edit {
        start,
        end,
        replacement: replacement.to_string(),
    }
}

fn emit(
    out: &mut Vec<Ablation>,
    rule: &str,
    qual: &str,
    ctx: &Ctx,
    n: Node,
    desc: String,
    edits: Vec<Edit>,
) {
    if edits.is_empty() {
        return;
    }
    out.push(Ablation {
        rule: rule.to_string(),
        qualname: qual.to_string(),
        line: ctx.line(n),
        description: desc,
        edits,
    });
}

/// The argument list of a call, split into positional, keyword, and splats.
///
/// A splat has to be reported separately: `f(*args)` has one child and an unknown
/// number of arguments, so any rule that reasons about argument *position* is
/// unsound in its presence and must decline rather than guess.
fn call_args<'t>(call: Node<'t>) -> Option<(Node<'t>, Vec<Node<'t>>, Vec<Node<'t>>, bool)> {
    let list = call.child_by_field_name("arguments")?;
    let (mut pos, mut kw) = (Vec::new(), Vec::new());
    let mut splat = false;
    for c in named(list) {
        match c.kind() {
            "keyword_argument" => kw.push(c),
            "list_splat" | "dictionary_splat" => splat = true,
            "comment" => {}
            _ => pos.push(c),
        }
    }
    Some((list, pos, kw, splat))
}

/// Span of a node plus the comma that separates it from its neighbour, so removing
/// an argument does not leave a dangling `,`.
fn span_with_comma(n: Node) -> (usize, usize) {
    if let Some(prev) = n.prev_sibling() {
        if prev.kind() == "," {
            return (prev.start_byte(), n.end_byte());
        }
    }
    if let Some(next) = n.next_sibling() {
        if next.kind() == "," {
            return (n.start_byte(), next.end_byte());
        }
    }
    (n.start_byte(), n.end_byte())
}

// ---------------------------------------------------------------- tensor methods

/// `x.clamp(min=1e-6)` -> `x`. Each method gets its own rule name so the per-rule
/// discrimination table stays readable; a single `drop_method` bucket hid the fact
/// that `drop_contiguous` never once produced a difference.
const METHODS: &[(&str, &str)] = &[
    ("clamp", "drop_clamp"),
    ("clamp_min", "drop_clamp_min"),
    ("clamp_max", "drop_clamp_max"),
    ("detach", "drop_detach"),
    ("contiguous", "drop_contiguous"),
    ("masked_fill", "drop_masked_fill"),
    ("masked_fill_", "drop_masked_fill_"),
    ("tril", "drop_tril"),
    ("triu", "drop_triu"),
    ("nan_to_num", "drop_nan_to_num"),
    ("softmax", "drop_softmax"),
    ("sigmoid", "drop_sigmoid"),
];

fn method_rule(ctx: &Ctx, n: Node, qual: &str, out: &mut Vec<Ablation>, want: &str, rule: &str) {
    if n.kind() != "call" {
        return;
    }
    let Some(f) = n.child_by_field_name("function") else {
        return;
    };
    if f.kind() != "attribute" {
        return;
    }
    let Some(attr) = f.child_by_field_name("attribute") else {
        return;
    };
    if ctx.text(attr) != want {
        return;
    }
    let Some(obj) = f.child_by_field_name("object") else {
        return;
    };
    let Some(recv) = inline(ctx.text(obj)) else {
        return;
    };
    emit(
        out,
        rule,
        qual,
        ctx,
        n,
        format!(".{want}(...) removed"),
        vec![edit(n.start_byte(), n.end_byte(), &recv)],
    );
}

macro_rules! method_rules {
    ($(($fname:ident, $m:expr)),* $(,)?) => {
        $(pub fn $fname(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
            let rule = METHODS.iter().find(|(m, _)| *m == $m).map(|(_, r)| *r).unwrap();
            method_rule(ctx, n, q, out, $m, rule);
        })*
    };
}
method_rules!(
    (r_clamp, "clamp"),
    (r_clamp_min, "clamp_min"),
    (r_clamp_max, "clamp_max"),
    (r_detach, "detach"),
    (r_contiguous, "contiguous"),
    (r_masked_fill, "masked_fill"),
    (r_masked_fill_, "masked_fill_"),
    (r_tril, "tril"),
    (r_triu, "triu"),
    (r_nan_to_num, "nan_to_num"),
    (r_softmax, "softmax"),
    (r_sigmoid, "sigmoid"),
);

// ---------------------------------------------------------------- expressions

/// `torch.where(c, a, b)` -> `a`: the condition stops selecting.
pub fn r_where(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "call" {
        return;
    }
    let Some(f) = n.child_by_field_name("function") else {
        return;
    };
    if !ctx.text(f).ends_with("where") {
        return;
    }
    let Some((_, pos, _, splat)) = call_args(n) else {
        return;
    };
    if pos.len() != 3 || splat {
        return;
    }
    let Some(keep) = inline(ctx.text(pos[1])) else {
        return;
    };
    emit(
        out,
        "drop_where",
        q,
        ctx,
        n,
        "where(...) always takes the true branch".into(),
        vec![edit(n.start_byte(), n.end_byte(), &keep)],
    );
}

/// `v / v.norm(...)` -> `v`.
pub fn r_normalisation(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "binary_operator" {
        return;
    }
    let (Some(l), Some(r)) = (
        n.child_by_field_name("left"),
        n.child_by_field_name("right"),
    ) else {
        return;
    };
    let op_is_div = n
        .child_by_field_name("operator")
        .map(|o| ctx.text(o) == "/")
        .unwrap_or_else(|| {
            let between = &ctx.src[l.end_byte()..r.start_byte()];
            String::from_utf8_lossy(between).contains('/')
        });
    if !op_is_div || !ctx.text(r).contains("norm") {
        return;
    }
    let Some(keep) = inline(ctx.text(l)) else {
        return;
    };
    emit(
        out,
        "drop_normalisation",
        q,
        ctx,
        n,
        "division by a norm removed".into(),
        vec![edit(n.start_byte(), n.end_byte(), &keep)],
    );
}

/// `if bad: raise ...` -> `pass`. A guard that no input reaches is dead weight;
/// a guard that fires is the only thing standing between a caller and corruption.
pub fn r_raise_guard(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "if_statement" || n.child_by_field_name("alternative").is_some() {
        return;
    }
    let Some(body) = n.child_by_field_name("consequence") else {
        return;
    };
    let stmts = named(body);
    if stmts.len() != 1 || stmts[0].kind() != "raise_statement" {
        return;
    }
    emit(
        out,
        "drop_raise_guard",
        q,
        ctx,
        n,
        "raise guard removed".into(),
        vec![edit(n.start_byte(), n.end_byte(), "pass")],
    );
}

/// A call whose result nobody reads: either it mutates something or it does nothing.
pub fn r_expression_statement(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "expression_statement" {
        return;
    }
    let kids = named(n);
    if kids.len() != 1 || kids[0].kind() != "call" {
        return;
    }
    emit(
        out,
        "drop_expression_statement",
        q,
        ctx,
        n,
        format!(
            "statement `{}` removed",
            ctx.text(kids[0]).chars().take(48).collect::<String>()
        ),
        vec![edit(n.start_byte(), n.end_byte(), "pass")],
    );
}

// ---------------------------------------------------------------- calls

/// `f(x)` -> `x`, but only when `f` is defined in this module. Firing on builtins
/// was the defect that invalidated an entire sweep: dropping an argument to
/// `isinstance` or `max` tests nothing about the code under review.
pub fn r_unary_call(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "call" {
        return;
    }
    let Some(f) = n.child_by_field_name("function") else {
        return;
    };
    if f.kind() != "identifier" || !ctx.funcs.contains_key(&ctx.text(f)) {
        return;
    }
    let Some((_, pos, kw, splat)) = call_args(n) else {
        return;
    };
    if pos.len() != 1 || !kw.is_empty() || splat {
        return;
    }
    let Some(keep) = inline(ctx.text(pos[0])) else {
        return;
    };
    emit(
        out,
        "drop_unary_call",
        q,
        ctx,
        n,
        format!("call to {} bypassed", ctx.text(f)),
        vec![edit(n.start_byte(), n.end_byte(), &keep)],
    );
}

/// Drop the last positional argument, but only where the callee declares a default
/// for that position — otherwise the mutant is a TypeError, not a question.
pub fn r_trailing_arg(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "call" {
        return;
    }
    let Some(f) = n.child_by_field_name("function") else {
        return;
    };
    if f.kind() != "identifier" {
        return;
    }
    let Some(info) = ctx.funcs.get(&ctx.text(f)) else {
        return;
    };
    let Some((_, pos, _, splat)) = call_args(n) else {
        return;
    };
    if splat {
        return;
    }
    let Some(last) = pos.last() else { return };
    match info.params.get(pos.len() - 1) {
        Some(p) if p.default.is_some() => {}
        _ => return,
    }
    let (s, e) = span_with_comma(*last);
    emit(
        out,
        "drop_trailing_arg",
        q,
        ctx,
        n,
        format!(
            "trailing argument to {} falls back to its default",
            ctx.text(f)
        ),
        vec![edit(s, e, "")],
    );
}

/// `f(..., alpha=2.0)` -> `f(...)`: the caller's override stops applying.
pub fn r_keyword_arg(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "call" {
        return;
    }
    let Some((_, _, kw, _)) = call_args(n) else {
        return;
    };
    for k in kw {
        let Some(name) = k.child_by_field_name("name") else {
            continue;
        };
        let (s, e) = span_with_comma(k);
        emit(
            out,
            "drop_keyword_arg",
            q,
            ctx,
            n,
            format!("keyword {} left at its default", ctx.text(name)),
            vec![edit(s, e, "")],
        );
    }
}

/// A decorator that changes nothing observable is a wrapper nobody needs.
pub fn r_decorator(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "decorator" {
        return;
    }
    // A decorator sits OUTSIDE the body it applies to -- it is a sibling of the
    // definition under `decorated_definition` -- so the walker has not pushed the
    // function's name onto the scope yet and `q` is only the ENCLOSING scope. Left
    // that way the ablation carried an empty qualname, and probe_function's
    // `qualname == target` filter discarded every one of them: the rule shipped but
    // never once reached the probe. Name the definition it decorates.
    let owner = n
        .parent()
        .and_then(|p| {
            named(p)
                .into_iter()
                .find(|c| matches!(c.kind(), "function_definition" | "class_definition"))
        })
        .and_then(|d| d.child_by_field_name("name"))
        .map(|name| ctx.text(name));
    let qual = match owner {
        Some(name) if q.is_empty() => name,
        Some(name) => format!("{q}.{name}"),
        None => q.to_string(),
    };
    let text = ctx.text(n);
    emit(
        out,
        "drop_decorator",
        &qual,
        ctx,
        n,
        format!("decorator {text} removed"),
        vec![edit(n.start_byte(), n.end_byte(), "")],
    );
}

// ---------------------------------------------------------------- signatures

fn is_reference(n: Node) -> bool {
    // Climb out of destructuring patterns first: in `a, name = f()` the identifier's
    // parent is a pattern list, not the assignment, so a parent-only check misses it.
    let mut cur = n;
    while let Some(p) = cur.parent() {
        match p.kind() {
            "pattern_list" | "tuple_pattern" | "list_pattern" => cur = p,
            _ => break,
        }
    }
    // `del a, b` nests the name under an expression list, so the parent check below
    // never sees the delete. Nothing under a delete is a read, at any depth.
    let mut up = Some(cur);
    while let Some(a) = up {
        match a.kind() {
            "delete_statement" => return false,
            "block" | "function_definition" | "module" => break,
            _ => up = a.parent(),
        }
    }
    let Some(p) = cur.parent() else { return true };
    let is_field = |f: &str| {
        p.child_by_field_name(f)
            .map(|a| a.id() == cur.id())
            .unwrap_or(false)
    };
    match p.kind() {
        // `x.name` -- the attribute half is not a variable.
        "attribute" => !p
            .child_by_field_name("attribute")
            .map(|a| a.id() == n.id())
            .unwrap_or(false),
        // `f(name=...)` -- the keyword half is not a variable.
        "keyword_argument" => !is_field("name"),
        // Every form that rebinds the name rather than reading it.
        "assignment" | "augmented_assignment" => !is_field("left"),
        "for_statement" | "for_in_clause" => !is_field("left"),
        "named_expression" => !is_field("name"),
        "as_pattern" => !is_field("alias"),
        "function_definition" | "class_definition" => !is_field("name"),
        "parameters"
        | "lambda_parameters"
        | "parameter"
        | "default_parameter"
        | "typed_default_parameter"
        | "typed_parameter"
        | "list_splat_pattern"
        | "dictionary_splat_pattern" => false,
        "global_statement" | "nonlocal_statement" | "delete_statement" => false,
        "dotted_name" | "aliased_import" | "import_statement" | "import_from_statement" => false,
        _ => true,
    }
}

fn find_uses<'t>(ctx: &Ctx, body: Node<'t>, name: &str) -> Vec<Node<'t>> {
    let mut hits = Vec::new();
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "identifier" && ctx.text(n) == name && is_reference(n) {
            hits.push(n);
        }
        stack.extend(named(n));
    }
    hits
}

/// Pin a defaulted parameter to its default at every use, leaving the signature
/// intact so no caller breaks. This asks the question a signature mutation should
/// ask -- does passing anything else matter? -- without turning the mutant into a
/// TypeError that every test kills for the wrong reason.
pub fn r_pin_parameter(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "function_definition" {
        return;
    }
    let Some(body) = n.child_by_field_name("body") else {
        return;
    };
    for p in params_of(ctx, n) {
        let Some(default) = p.default.clone() else {
            continue;
        };
        let uses = find_uses(ctx, body, &p.name);
        if uses.is_empty() {
            continue;
        }
        let edits = uses
            .iter()
            .map(|u| edit(u.start_byte(), u.end_byte(), &default))
            .collect();
        emit(
            out,
            "pin_parameter_to_default",
            q,
            ctx,
            n,
            format!("parameter {} pinned to {}", p.name, default),
            edits,
        );
    }
}

/// Flip a boolean default. The caller that relies on it will notice; nobody else will.
pub fn r_boolean_default(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "default_parameter" && n.kind() != "typed_default_parameter" {
        return;
    }
    let (Some(name), Some(v)) = (
        n.child_by_field_name("name"),
        n.child_by_field_name("value"),
    ) else {
        return;
    };
    let flipped = match v.kind() {
        "true" => "False",
        "false" => "True",
        _ => return,
    };
    emit(
        out,
        "flip_boolean_default",
        q,
        ctx,
        n,
        format!("default of {} flipped to {}", ctx.text(name), flipped),
        vec![edit(v.start_byte(), v.end_byte(), flipped)],
    );
}

// ---------------------------------------------------------------- imports

/// Remove a module-level import. Two oracles are required to read the result:
/// a re-export is load-bearing for its consumers and invisible to its own tests,
/// so a behaviour-only verdict calls every re-export dead.
pub fn r_import(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "import_statement" && n.kind() != "import_from_statement" {
        return;
    }
    if n.parent().map(|p| p.kind() != "module").unwrap_or(true) {
        return;
    }
    let line = ctx.line(n);
    let tag = if ctx.silenced.contains(&line) {
        " (suppressed)"
    } else {
        ""
    };
    emit(
        out,
        "drop_import",
        q,
        ctx,
        n,
        format!(
            "import removed{tag}: {}",
            ctx.text(n).lines().next().unwrap_or("")
        ),
        vec![edit(n.start_byte(), n.end_byte(), "pass")],
    );
}

// ---------------------------------------------------------------- procedures

fn body_indent(ctx: &Ctx, body: Node) -> String {
    " ".repeat(ctx.col(body).max(4))
}

fn is_trivial(ctx: &Ctx, body: Node) -> bool {
    let stmts = named(body);
    stmts.iter().all(|s| {
        s.kind() == "pass_statement"
            || (s.kind() == "expression_statement"
                && named(*s)
                    .first()
                    .map(|c| c.kind() == "string")
                    .unwrap_or(false))
    }) || stmts.is_empty()
        || ctx.text(body).len() < 12
}

/// Replace a whole function body with `return None`. This is the strongest question
/// the framework can ask -- does this procedure do anything anyone can observe? --
/// and it is the one the Python engine could not express.
pub fn r_ablate_to_none(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "function_definition" {
        return;
    }
    let Some(body) = n.child_by_field_name("body") else {
        return;
    };
    if is_trivial(ctx, body) {
        return;
    }
    let repl = format!("\n{}return None", body_indent(ctx, body));
    emit(
        out,
        "ablate_function_to_none",
        q,
        ctx,
        n,
        "entire body replaced by `return None`".into(),
        vec![edit(body.start_byte(), body.end_byte(), &repl)],
    );
}

/// Return the first real argument unchanged. Measured once and never discriminated,
/// so it is opt-in rather than deleted -- it may yet earn its place on other code.
pub fn r_ablate_to_passthrough(ctx: &Ctx, n: Node, q: &str, out: &mut Vec<Ablation>) {
    if n.kind() != "function_definition" {
        return;
    }
    let Some(body) = n.child_by_field_name("body") else {
        return;
    };
    if is_trivial(ctx, body) {
        return;
    }
    let params = params_of(ctx, n);
    let Some(first) = params
        .iter()
        .find(|p| p.name != "self" && p.name != "cls" && !p.name.starts_with('*'))
    else {
        return;
    };
    let repl = format!("\n{}return {}", body_indent(ctx, body), first.name);
    emit(
        out,
        "ablate_function_to_passthrough",
        q,
        ctx,
        n,
        format!("entire body replaced by `return {}`", first.name),
        vec![edit(body.start_byte(), body.end_byte(), &repl)],
    );
}

// ---------------------------------------------------------------- registry

pub const DEFAULT: &[(&str, Rule)] = &[
    ("drop_clamp", r_clamp),
    ("drop_clamp_min", r_clamp_min),
    ("drop_clamp_max", r_clamp_max),
    ("drop_detach", r_detach),
    ("drop_masked_fill", r_masked_fill),
    ("drop_masked_fill_", r_masked_fill_),
    ("drop_tril", r_tril),
    ("drop_triu", r_triu),
    ("drop_nan_to_num", r_nan_to_num),
    ("drop_softmax", r_softmax),
    ("drop_sigmoid", r_sigmoid),
    ("drop_where", r_where),
    ("drop_normalisation", r_normalisation),
    ("drop_raise_guard", r_raise_guard),
    ("drop_expression_statement", r_expression_statement),
    ("drop_unary_call", r_unary_call),
    ("drop_trailing_arg", r_trailing_arg),
    ("drop_keyword_arg", r_keyword_arg),
    ("drop_decorator", r_decorator),
    ("pin_parameter_to_default", r_pin_parameter),
    ("flip_boolean_default", r_boolean_default),
    ("drop_import", r_import),
    ("ablate_function_to_none", r_ablate_to_none),
];

/// Measured, never discriminated. Kept behind an explicit opt-in.
pub const OPTIONAL: &[(&str, Rule)] = &[
    ("drop_contiguous", r_contiguous),
    ("ablate_function_to_passthrough", r_ablate_to_passthrough),
];
