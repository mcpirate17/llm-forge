use super::{array, string, Finding};
use regex::Regex;
use ruff_python_ast as ast;
use ruff_python_ast::token::TokenKind;
use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use ruff_text_size::Ranged;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::LazyLock;

static STUB_COMMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:TODO|FIXME|XXX)\b|pragma:\s*no cover").unwrap());
static SOFTMAX_FALLBACK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:fallback.{0,80}(?:softmax|attention)|(?:softmax|attention).{0,80}fallback|scaled_dot_product_attention|MultiheadAttention)").unwrap()
});
static RANDOM_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:random\.|np\.random|torch\.rand|torch\.randn)").unwrap());
static SEED_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:seed|manual_seed|Generator)\b").unwrap());

struct Lines {
    starts: Vec<usize>,
}

impl Lines {
    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        Self { starts }
    }

    fn number(&self, offset: usize) -> usize {
        self.starts.partition_point(|start| *start <= offset)
    }

    fn column(&self, offset: usize) -> usize {
        offset - self.starts[self.number(offset).saturating_sub(1)]
    }
}

fn call_name(expression: &ast::Expr) -> String {
    match expression {
        ast::Expr::Name(node) => node.id.as_str().to_owned(),
        ast::Expr::Attribute(node) => {
            let prefix = call_name(&node.value);
            if prefix.is_empty() {
                String::new()
            } else {
                format!("{prefix}.{}", node.attr)
            }
        }
        _ => String::new(),
    }
}

fn decorator_name(expression: &ast::Expr) -> String {
    if let ast::Expr::Call(call) = expression {
        call_name(&call.func)
    } else {
        call_name(expression)
    }
}

fn device_kernel(function: &ast::StmtFunctionDef) -> bool {
    function.decorator_list.iter().any(|decorator| {
        matches!(
            decorator_name(&decorator.expression).as_str(),
            "triton.jit"
                | "jit"
                | "triton.autotune"
                | "triton.heuristics"
                | "numba.njit"
                | "numba.jit"
                | "numba.cuda.jit"
        )
    })
}

struct Visitor<'a> {
    path: &'a str,
    lines: &'a Lines,
    hot: bool,
    findings: Vec<Finding>,
    loop_depth: usize,
    protocol_depth: usize,
    kernel_depth: usize,
}

impl Visitor<'_> {
    fn add(
        &mut self,
        rule: &'static str,
        severity: &'static str,
        offset: usize,
        message: impl Into<String>,
    ) {
        self.findings.push(
            Finding::new("python-ast", rule, severity, message)
                .path(self.path)
                .line(self.lines.number(offset))
                .column(self.lines.column(offset)),
        );
    }

    fn visit_function(&mut self, function: &ast::StmtFunctionDef) {
        // Ruff's statement range can start at a decorator. CPython locates a
        // FunctionDef at `def` (or `async def`), including for decorated code.
        let start = function
            .name
            .range
            .start()
            .to_usize()
            .saturating_sub(if function.is_async {
                "async def ".len()
            } else {
                "def ".len()
            });
        let end = function.range.end().to_usize().saturating_sub(1);
        let length = self.lines.number(end) - self.lines.number(start) + 1;
        if length > 100 {
            self.add(
                "oversized-function",
                "high",
                start,
                format!("function is {length} lines (>100)"),
            );
        }
        if function.body.len() != 1 {
            return;
        }
        match &function.body[0] {
            ast::Stmt::Pass(node) => self.add(
                "pass-stub",
                "high",
                node.range.start().to_usize(),
                "pass-only function is a partial implementation",
            ),
            ast::Stmt::Expr(node)
                if matches!(node.value.as_ref(), ast::Expr::EllipsisLiteral(_))
                    && self.protocol_depth == 0 =>
            {
                self.add(
                    "ellipsis-stub",
                    "high",
                    node.range.start().to_usize(),
                    "ellipsis-only function is a stub",
                );
            }
            _ => {}
        }
    }

    fn visit_call(&mut self, call: &ast::ExprCall) {
        let name = call_name(&call.func);
        let offset = call.range.start().to_usize();
        let flagged = match call.func.as_ref() {
            ast::Expr::Name(node) if matches!(node.id.as_str(), "eval" | "exec") => {
                Some(node.id.as_str())
            }
            _ if name == "os.system" => Some("os.system"),
            _ => None,
        };
        if let Some(flagged) = flagged {
            self.add(
                "dynamic-execution",
                "critical",
                offset,
                format!("unsafe dynamic execution via {flagged}"),
            );
        }
        if matches!(
            name.as_str(),
            "pickle.load" | "pickle.loads" | "dill.load" | "dill.loads"
        ) {
            self.add(
                "unsafe-deserialization",
                "critical",
                offset,
                format!("unsafe deserialization via {name}"),
            );
        }
        if name == "yaml.load"
            && !call.arguments.keywords.iter().any(|keyword| {
                keyword
                    .arg
                    .as_ref()
                    .is_some_and(|arg| arg.as_str() == "Loader")
            })
        {
            self.add(
                "unsafe-yaml",
                "critical",
                offset,
                "yaml.load without an explicit safe loader",
            );
        }
        self.call_shell(&name, call, offset);
        self.call_repeated(&name, offset);
    }

    fn call_shell(&mut self, name: &str, call: &ast::ExprCall, offset: usize) {
        if !matches!(
            name,
            "subprocess.run" | "subprocess.call" | "subprocess.Popen" | "os.popen"
        ) {
            return;
        }
        let shell_true = call.arguments.keywords.iter().any(|keyword| {
            keyword
                .arg
                .as_ref()
                .is_some_and(|arg| arg.as_str() == "shell")
                && matches!(&keyword.value, ast::Expr::BooleanLiteral(value) if value.value)
        });
        if shell_true || name == "os.popen" {
            self.add(
                "unsafe-shell",
                "high",
                offset,
                "shell execution is injection-prone",
            );
        }
    }

    fn call_repeated(&mut self, name: &str, offset: usize) {
        let repeated = self.loop_depth >= 2
            && matches!(
                name,
                "json.load" | "json.loads" | "Path.read_text" | "Path.read_bytes" | "open"
            );
        if repeated || (self.loop_depth >= 1 && name == "re.compile") {
            let severity = if self.hot { "high" } else { "medium" };
            self.add(
                "repeated-parsing-io",
                severity,
                offset,
                format!("{name} inside a loop causes repeated parsing or I/O"),
            );
        }
    }
}

impl<'a> SourceOrderVisitor<'a> for Visitor<'_> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        match statement {
            ast::Stmt::FunctionDef(function) => {
                self.visit_function(function);
                let kernel = device_kernel(function);
                if kernel {
                    self.kernel_depth += 1;
                }
                // ast.NodeVisitor traverses FunctionDef fields in this order.
                // SourceOrderVisitor would visit decorators first, changing the
                // ordered finding list and therefore the review receipt.
                self.visit_parameters(&function.parameters);
                self.visit_body(&function.body);
                for decorator in &function.decorator_list {
                    self.visit_decorator(decorator);
                }
                if let Some(returns) = function.returns.as_deref() {
                    self.visit_expr(returns);
                }
                if let Some(type_params) = function.type_params.as_deref() {
                    self.visit_type_params(type_params);
                }
                if kernel {
                    self.kernel_depth -= 1;
                }
                return;
            }
            ast::Stmt::ClassDef(class) => {
                let previous = self.protocol_depth;
                self.protocol_depth = usize::from(class.arguments.as_ref().is_some_and(|args| {
                    args.args
                        .iter()
                        .any(|base| call_name(base).rsplit('.').next() == Some("Protocol"))
                }));
                if let Some(arguments) = class.arguments.as_deref() {
                    self.visit_arguments(arguments);
                }
                self.visit_body(&class.body);
                for decorator in &class.decorator_list {
                    self.visit_decorator(decorator);
                }
                if let Some(type_params) = class.type_params.as_deref() {
                    self.visit_type_params(type_params);
                }
                self.protocol_depth = previous;
                return;
            }
            ast::Stmt::For(node) => {
                self.loop_depth += 1;
                if self.loop_depth >= 2 && self.hot && self.kernel_depth == 0 {
                    self.add("nested-loop-hotpath", "medium", node.range.start().to_usize(),
                        "nested Python loop in a high-risk path needs a measured complexity budget or vectorized/native path");
                }
                walk_stmt(self, statement);
                self.loop_depth -= 1;
                return;
            }
            ast::Stmt::While(_) => {
                self.loop_depth += 1;
                walk_stmt(self, statement);
                self.loop_depth -= 1;
                return;
            }
            ast::Stmt::Raise(node) => {
                let raised = node.exc.as_deref().map(|expr| {
                    if let ast::Expr::Call(call) = expr {
                        call.func.as_ref()
                    } else {
                        expr
                    }
                });
                if raised.is_some_and(|expr| call_name(expr) == "NotImplementedError") {
                    self.add(
                        "not-implemented-stub",
                        "high",
                        node.range.start().to_usize(),
                        "NotImplementedError is a partial implementation",
                    );
                }
            }
            _ => {}
        }
        walk_stmt(self, statement);
    }

    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        if let ast::Expr::Call(call) = expression {
            self.visit_call(call);
        }
        walk_expr(self, expression);
    }
}

fn changed_text(lines: &[Value], changed: &[usize]) -> String {
    changed
        .iter()
        .filter_map(|number| number.checked_sub(1).and_then(|index| lines.get(index)))
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

fn stub_comment_lines(
    parsed: &ruff_python_parser::Parsed<ast::ModModule>,
    source: &str,
    lines: &Lines,
    changed: &HashSet<usize>,
) -> Vec<usize> {
    parsed
        .tokens()
        .iter()
        .filter(|token| token.kind() == TokenKind::Comment)
        .filter_map(|token| {
            let offset = token.range().start().to_usize();
            let line = lines.number(offset);
            (changed.contains(&line)
                && source
                    .get(token.range().start().to_usize()..token.range().end().to_usize())
                    .is_some_and(|comment| STUB_COMMENT.is_match(comment)))
            .then_some(line)
        })
        .collect()
}

pub fn python_ast(payload: &Value) -> Result<Value, String> {
    let path = string(payload, "path")?;
    let source = string(payload, "text")?;
    let text_lines = array(payload, "lines")?;
    let changed_numbers: HashSet<usize> = array(payload, "changed_lines")?
        .iter()
        .map(|number| {
            number
                .as_u64()
                .ok_or("changed line must be an integer")
                .map(|number| number as usize)
        })
        .collect::<Result<_, _>>()?;
    let mut changed: Vec<usize> = changed_numbers.iter().copied().collect();
    changed.sort_unstable();
    let parsed = ruff_python_parser::parse_module(source).map_err(|error| error.to_string())?;
    let lines = Lines::new(source);
    let mut findings = Vec::new();
    if text_lines.len() > 1250 {
        findings.push(
            Finding::new(
                "python-ast",
                "oversized-module",
                "high",
                format!("module is {} lines (>1250)", text_lines.len()),
            )
            .path(path),
        );
    }
    let mut visitor = Visitor {
        path,
        lines: &lines,
        hot: payload["hot"].as_bool().unwrap_or(false),
        findings: Vec::new(),
        loop_depth: 0,
        protocol_depth: 0,
        kernel_depth: 0,
    };
    for statement in parsed.suite() {
        visitor.visit_stmt(statement);
    }
    findings.extend(visitor.findings);
    let changed_text = changed_text(text_lines, &changed);
    let comments = stub_comment_lines(&parsed, source, &lines, &changed_numbers);
    let classes = array(payload, "classes")?;
    let is_test = classes.iter().any(|class| class == "test");
    let is_novel = classes.iter().any(|class| class == "novel");
    if !is_test {
        if let Some(first) = comments.first() {
            findings.push(
                Finding::new(
                    "python-ast",
                    "partial-implementation-marker",
                    "high",
                    "changed production code contains TODO/stub/coverage-bypass scaffolding",
                )
                .path(path)
                .line(*first),
            );
        }
    }
    if is_novel && !is_test {
        if let Some(matched) = SOFTMAX_FALLBACK.find(&changed_text) {
            findings.push(Finding::new("python-ast", "softmax-shaped-fallback", "critical",
                "novel-mechanism path adds an attention/softmax-shaped fallback; fix the novel branch instead of masking it")
                .path(path).line(lines_count(&changed_text, matched.start())));
        }
    }
    if RANDOM_TEXT.is_match(&changed_text) && !SEED_TEXT.is_match(source) {
        findings.push(
            Finding::new(
                "python-ast",
                "nondeterministic-research",
                "high",
                "randomized changed code has no explicit deterministic seed path",
            )
            .path(path),
        );
    }
    let promotion_path = ["promotion", "program_write", "result_record"]
        .iter()
        .any(|token| path.contains(token));
    let metric_contract = ["wikitext", "hellaswag", "blimp", "binding"]
        .iter()
        .all(|token| source.contains(token));
    if promotion_path && changed_text.contains("stage1_passed") && !metric_contract {
        findings.push(
            Finding::new(
                "python-ast",
                "partial-promotion-write",
                "critical",
                "promotion/write path changes stage-1 success without the complete metric contract",
            )
            .path(path),
        );
    }
    Ok(json!({"findings": findings}))
}

fn lines_count(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}
