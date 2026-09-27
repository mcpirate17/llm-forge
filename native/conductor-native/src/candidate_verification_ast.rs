//! Collectible Python definitions with decorator-inclusive source identity.

use ruff_python_ast as ast;
use ruff_text_size::Ranged;
use serde_json::{Map, Value};

struct Lines<'a> {
    text: Vec<&'a str>,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(source: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        Self {
            text: source.lines().collect(),
            starts,
        }
    }

    fn number(&self, offset: usize) -> usize {
        self.starts.partition_point(|start| *start <= offset)
    }

    fn definition(&self, function: &ast::StmtFunctionDef) -> String {
        let start = self.number(function.range().start().to_usize());
        let end = self.number(function.range().end().to_usize().saturating_sub(1));
        self.text[start.saturating_sub(1)..end.min(self.text.len())].join("\n")
    }
}

fn add_function(
    definitions: &mut Map<String, Value>,
    lines: &Lines<'_>,
    function: &ast::StmtFunctionDef,
    class: Option<&str>,
) {
    let name = function.name.as_str();
    if !name.starts_with("test_") {
        return;
    }
    let label = class.map_or_else(|| name.to_owned(), |class| format!("{class}::{name}"));
    definitions.insert(label, Value::String(lines.definition(function)));
}

pub(super) fn python_test_definitions(source: &str, path: &str) -> Result<Value, String> {
    let module = ruff_python_parser::parse_module(source)
        .map_err(|error| format!("cannot parse test definitions in {path}: {error}"))?
        .into_syntax();
    let lines = Lines::new(source);
    let mut definitions = Map::new();
    for statement in &module.body {
        match statement {
            ast::Stmt::FunctionDef(function) => {
                add_function(&mut definitions, &lines, function, None);
            }
            ast::Stmt::ClassDef(class) if class.name.as_str().starts_with("Test") => {
                for child in &class.body {
                    if let ast::Stmt::FunctionDef(function) = child {
                        add_function(
                            &mut definitions,
                            &lines,
                            function,
                            Some(class.name.as_str()),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    Ok(Value::Object(definitions))
}
