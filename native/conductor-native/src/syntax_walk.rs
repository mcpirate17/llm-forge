use super::{Dependency, Inspection, Language, Reference, Symbol, MAX_AST_DEPTH, MAX_AST_NODES};
use anyhow::{ensure, Result};
use std::collections::HashSet;
use tree_sitter::{Node, Tree};

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn named_definition(node: Node<'_>) -> Option<(&'static str, Node<'_>, Option<Node<'_>>)> {
    let kind = match node.kind() {
        "function_item"
        | "function_definition"
        | "function_declaration"
        | "method_definition"
        | "function_signature_item"
        | "method_signature" => "Function",
        "class_definition"
        | "class_declaration"
        | "struct_item"
        | "struct_specifier"
        | "class_specifier"
        | "enum_item"
        | "enum_specifier"
        | "interface_declaration" => "Class",
        "namespace_definition" | "mod_item" => "Module",
        "impl_item" => "Implementation",
        "variable_declarator" => {
            let value = node.child_by_field_name("value")?;
            if !matches!(value.kind(), "arrow_function" | "function_expression") {
                return None;
            }
            return Some((
                "Function",
                node.child_by_field_name("name")?,
                value.child_by_field_name("body"),
            ));
        }
        _ => return None,
    };
    let name = if node.kind() == "impl_item" {
        node.child_by_field_name("type")
    } else {
        node.child_by_field_name("name")
            .or_else(|| declarator_name(node.child_by_field_name("declarator")?))
    }?;
    Some((kind, name, node.child_by_field_name("body")))
}

fn declarator_name(mut node: Node<'_>) -> Option<Node<'_>> {
    for _ in 0..32 {
        if matches!(
            node.kind(),
            "identifier"
                | "field_identifier"
                | "qualified_identifier"
                | "destructor_name"
                | "operator_name"
        ) {
            return Some(node);
        }
        node = node.child_by_field_name("declarator")?;
    }
    None
}

fn static_call(target: &str) -> bool {
    !target.is_empty()
        && target
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ':'))
}

pub(super) fn inspect(source: &str, language: Language, tree: &Tree) -> Result<Inspection> {
    let mut result = Inspection {
        language,
        source_sha256: super::fingerprint(source),
        symbols: Vec::new(),
        references: Vec::new(),
        dependencies: Vec::new(),
        dynamic_calls: 0,
    };
    let mut stack = vec![(tree.root_node(), None, 0)];
    let mut names = HashSet::new();
    let mut visited = 0;
    let mut owned_bytes = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    while let Some((node, parent, depth)) = stack.pop() {
        visited += 1;
        ensure!(
            visited <= MAX_AST_NODES && depth <= MAX_AST_DEPTH,
            "AST node/depth limit exceeded"
        );
        if visited % 256 == 0 {
            ensure!(
                std::time::Instant::now() < deadline,
                "syntax inspection exceeded 250 ms budget"
            );
        }
        let before = (
            result.symbols.len(),
            result.references.len(),
            result.dependencies.len(),
        );
        let scope = parent.map_or("", |i: usize| result.symbols[i].qualified.as_str());
        let mut child_scope = parent;
        if let Some((kind, name, body)) = named_definition(node) {
            let name_text = text(name, source).to_owned();
            let qualified = if scope.is_empty() {
                name_text.clone()
            } else {
                format!("{scope}{}{name_text}", language.scope_separator())
            };
            names.insert((name.start_byte(), name.end_byte()));
            child_scope = Some(result.symbols.len());
            result.symbols.push(Symbol {
                name: name_text,
                qualified,
                kind: kind.to_owned(),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                byte_start: node.start_byte(),
                byte_end: node.end_byte(),
                signature: source
                    [node.start_byte()..body.map_or(node.end_byte(), |b| b.start_byte())]
                    .trim_end()
                    .to_owned(),
                body: body.map(|b| (b.start_byte(), b.end_byte())),
            });
        }
        let scope = parent.map_or("", |i| result.symbols[i].qualified.as_str());
        result.dynamic_calls += reference(node, source, scope, &names, &mut result.references);
        dependency(node, source, &mut result);
        owned_bytes += added_bytes(&result, before);
        ensure!(
            owned_bytes <= 8 * 1024 * 1024
                && result.symbols.len() <= 20_000
                && result.references.len() <= 100_000,
            "syntax output exceeds bounded inspection storage"
        );
        for index in (0..node.named_child_count()).rev() {
            let child = node
                .named_child(index as u32)
                .expect("named child index is in range");
            stack.push((child, child_scope, depth + 1));
        }
    }
    Ok(result)
}

fn reference(
    node: Node<'_>,
    source: &str,
    scope: &str,
    names: &HashSet<(usize, usize)>,
    references: &mut Vec<Reference>,
) -> usize {
    if matches!(node.kind(), "call_expression" | "call") {
        if let Some(function) = node.child_by_field_name("function") {
            let target = text(function, source);
            if static_call(target) {
                references.push(Reference {
                    name: target.to_owned(),
                    scope: scope.to_owned(),
                    role: "call".to_owned(),
                    line: node.start_position().row + 1,
                    byte_start: function.start_byte(),
                    byte_end: function.end_byte(),
                });
            } else {
                return 1;
            }
        } else {
            return 1;
        }
    } else if matches!(
        node.kind(),
        "identifier" | "field_identifier" | "type_identifier" | "property_identifier"
    ) && !names.contains(&(node.start_byte(), node.end_byte()))
    {
        references.push(Reference {
            name: text(node, source).to_owned(),
            scope: scope.to_owned(),
            role: if node.kind() == "type_identifier" {
                "type"
            } else {
                "identifier"
            }
            .to_owned(),
            line: node.start_position().row + 1,
            byte_start: node.start_byte(),
            byte_end: node.end_byte(),
        });
    }
    0
}

fn added_bytes(result: &Inspection, before: (usize, usize, usize)) -> usize {
    result.symbols[before.0..]
        .iter()
        .map(|s| s.name.len() + s.qualified.len() + s.kind.len() + s.signature.len())
        .sum::<usize>()
        + result.references[before.1..]
            .iter()
            .map(|r| r.name.len() + r.scope.len() + r.role.len())
            .sum::<usize>()
        + result.dependencies[before.2..]
            .iter()
            .map(|d| d.target.len() + d.kind.len())
            .sum::<usize>()
}

fn dependency(node: Node<'_>, source: &str, result: &mut Inspection) {
    if result.language == Language::Python && node.kind() == "import_statement" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let name = if child.kind() == "aliased_import" {
                child.child_by_field_name("name").unwrap_or(child)
            } else {
                child
            };
            result.dependencies.push(Dependency {
                target: text(name, source).to_owned(),
                kind: "import_statement".to_owned(),
                line: node.start_position().row + 1,
            });
        }
        return;
    }
    let target = match node.kind() {
        "preproc_include" => node.child_by_field_name("path"),
        "import_statement" | "export_statement" => node.child_by_field_name("source"),
        "import_from_statement" => node.child_by_field_name("module_name"),
        "use_declaration" => node.child_by_field_name("argument"),
        _ => None,
    };
    if let Some(target) = target {
        result.dependencies.push(Dependency {
            target: text(target, source)
                .trim_matches(['"', '\'', '<', '>'])
                .to_owned(),
            kind: node.kind().to_owned(),
            line: node.start_position().row + 1,
        });
    }
}
