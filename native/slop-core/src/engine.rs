//! Parse Python once, walk it once, emit byte-span edits.
//!
//! An ablation is a set of edits rather than a single span because the signature
//! rules must rewrite every use of a parameter along with the parameter itself;
//! a one-span representation cannot express that and the Python version could not
//! carry those rules at all.

use tree_sitter::{Node, Parser};

#[derive(Clone, Debug)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

#[derive(Clone, Debug)]
pub struct Ablation {
    pub rule: String,
    pub qualname: String,
    pub line: usize,
    pub description: String,
    pub edits: Vec<Edit>,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub default: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FuncInfo {
    pub params: Vec<Param>,
}

/// Everything a rule needs that is not the node itself.
pub struct Ctx<'a> {
    pub src: &'a [u8],
    /// Module-level `def`s, so the call rules can refuse to fire on builtins.
    /// Dropping an argument to `isinstance` says nothing; dropping one to a
    /// local function with a default for it is a real question.
    pub funcs: std::collections::HashMap<String, FuncInfo>,
    /// Lines carrying a `# noqa` suppression, for the import rule.
    pub silenced: std::collections::HashSet<usize>,
}

impl<'a> Ctx<'a> {
    pub fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.src[n.byte_range()]).into_owned()
    }
    pub fn line(&self, n: Node) -> usize {
        n.start_position().row + 1
    }
    pub fn col(&self, n: Node) -> usize {
        n.start_position().column
    }
}

pub fn parse(source: &str) -> Option<tree_sitter::Tree> {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_python::language()).ok()?;
    p.parse(source, None)
}

/// Named children only — tree-sitter emits punctuation as anonymous nodes and no
/// rule wants to see a comma as a candidate.
pub fn named(n: Node) -> Vec<Node> {
    let mut c = n.walk();
    n.named_children(&mut c).collect()
}

fn param_of(ctx: &Ctx, n: Node) -> Option<Param> {
    match n.kind() {
        "identifier" => Some(Param {
            name: ctx.text(n),
            default: None,
        }),
        "default_parameter" | "typed_default_parameter" => {
            let name = n.child_by_field_name("name")?;
            let val = n.child_by_field_name("value")?;
            Some(Param {
                name: ctx.text(name),
                default: Some(ctx.text(val)),
            })
        }
        "typed_parameter" => {
            let inner = named(n).into_iter().next()?;
            Some(Param {
                name: ctx.text(inner),
                default: None,
            })
        }
        _ => None,
    }
}

pub fn params_of(ctx: &Ctx, func: Node) -> Vec<Param> {
    func.child_by_field_name("parameters")
        .map(|ps| named(ps).iter().filter_map(|c| param_of(ctx, *c)).collect())
        .unwrap_or_default()
}

/// Index every `def` in the module, at any nesting depth, by bare name.
fn index_funcs(src: &[u8], root: Node) -> std::collections::HashMap<String, FuncInfo> {
    let probe = Ctx {
        src,
        funcs: Default::default(),
        silenced: Default::default(),
    };
    let mut out = std::collections::HashMap::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_definition" {
            if let Some(name) = n.child_by_field_name("name") {
                out.insert(
                    probe.text(name),
                    FuncInfo {
                        params: params_of(&probe, n),
                    },
                );
            }
        }
        stack.extend(named(n));
    }
    out
}

fn silenced_lines(source: &str) -> std::collections::HashSet<usize> {
    source
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("# noqa"))
        .map(|(i, _)| i + 1)
        .collect()
}

pub type Rule = fn(&Ctx, Node, &str, &mut Vec<Ablation>);

/// Walk once, offering every node to every enabled rule. One traversal for the
/// whole rule set is the point: the Python version re-walked per rule.
pub fn collect(source: &str, rules: &[(&str, Rule)]) -> Vec<Ablation> {
    let Some(tree) = parse(source) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let src = source.as_bytes();
    let ctx = Ctx {
        src,
        funcs: index_funcs(src, root),
        silenced: silenced_lines(source),
    };
    let mut out = Vec::new();
    walk(&ctx, root, &mut Vec::new(), rules, &mut out);
    out.sort_by_key(|a| (a.line, a.rule.clone()));
    out
}

fn walk(
    ctx: &Ctx,
    n: Node,
    scope: &mut Vec<String>,
    rules: &[(&str, Rule)],
    out: &mut Vec<Ablation>,
) {
    let pushed = match n.kind() {
        "function_definition" | "class_definition" => n
            .child_by_field_name("name")
            .map(|name| {
                scope.push(ctx.text(name));
                true
            })
            .unwrap_or(false),
        _ => false,
    };
    let qual = scope.join(".");
    for (_, rule) in rules {
        rule(ctx, n, &qual, out);
    }
    for child in named(n) {
        walk(ctx, child, scope, rules, out);
    }
    if pushed {
        scope.pop();
    }
}

/// Apply edits back-to-front so earlier offsets stay valid.
pub fn apply(source: &str, ablation: &Ablation) -> String {
    let mut edits = ablation.edits.clone();
    edits.sort_by_key(|e| std::cmp::Reverse(e.start));
    let mut s = source.to_string();
    for e in edits {
        if e.start <= e.end && e.end <= s.len() {
            s.replace_range(e.start..e.end, &e.replacement);
        }
    }
    s
}
