use ruff_python_ast::{
    self as ast,
    visitor::source_order::{walk_stmt, SourceOrderVisitor},
};
use std::collections::BTreeSet;

fn guard(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Name(n) => n.id.as_str() == "TYPE_CHECKING",
        ast::Expr::Attribute(a) => a.attr.as_str() == "TYPE_CHECKING",
        _ => false,
    }
}

struct Imports {
    runtime: bool,
    names: BTreeSet<String>,
}
impl<'a> SourceOrderVisitor<'a> for Imports {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        match statement {
            ast::Stmt::Import(import) => {
                for alias in &import.names {
                    self.names
                        .insert(alias.name.as_str().split('.').next().unwrap_or("").into());
                }
            }
            ast::Stmt::ImportFrom(import) if import.level == 0 => {
                if let Some(name) = &import.module {
                    self.names
                        .insert(name.as_str().split('.').next().unwrap_or("").into());
                }
            }
            ast::Stmt::If(branch) if self.runtime => {
                if !guard(&branch.test) {
                    for statement in &branch.body {
                        self.visit_stmt(statement);
                    }
                }
                for clause in &branch.elif_else_clauses {
                    if clause.test.as_ref().is_none_or(|test| !guard(test)) {
                        for statement in &clause.body {
                            self.visit_stmt(statement);
                        }
                    }
                }
            }
            _ => walk_stmt(self, statement),
        }
    }
}

const MAX_AGGREGATE_NODES: usize = 1_000_000;

fn admit_nodes(total: &mut usize, nodes: usize) -> Result<(), String> {
    *total = total
        .checked_add(nodes)
        .filter(|n| *n <= MAX_AGGREGATE_NODES)
        .ok_or("aggregate Python AST exceeds 1000000 nodes")?;
    Ok(())
}

pub(super) fn imports(
    text: &str,
    total_nodes: &mut usize,
) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let document = crate::candidate_python::Document::parse(text).map_err(|e| e.to_string())?;
    admit_nodes(total_nodes, document.nodes())?;
    let mut all = Imports {
        runtime: false,
        names: BTreeSet::new(),
    };
    let mut runtime = Imports {
        runtime: true,
        names: BTreeSet::new(),
    };
    for stmt in &document.module().body {
        all.visit_stmt(stmt);
        runtime.visit_stmt(stmt);
    }
    Ok((all.names, runtime.names))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregate_node_limit_is_checked_without_overflow() {
        let mut nodes = MAX_AGGREGATE_NODES - 1;
        assert!(admit_nodes(&mut nodes, 1).is_ok());
        assert!(admit_nodes(&mut nodes, 1).is_err());
        let mut overflow = usize::MAX;
        assert!(admit_nodes(&mut overflow, 1).is_err());
    }
}
