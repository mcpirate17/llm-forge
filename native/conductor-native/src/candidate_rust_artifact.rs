//! Bounded Rust source projection used only to compare production build inputs.
//! Unsupported cfg shapes are retained, and malformed/ambiguous inputs refuse.
use crate::syntax::{self, Language};
use tree_sitter::Node;

const MAX_NODES: usize = 65_536;
const MAX_DEPTH: usize = 128;

fn comment(node: Node<'_>) -> bool {
    matches!(node.kind(), "line_comment" | "block_comment")
}

fn doc(node: Node<'_>, text: &str) -> bool {
    let raw = &text[node.byte_range()];
    (raw.starts_with("///") && !raw.starts_with("////"))
        || (raw.starts_with("/**") && !raw.starts_with("/***"))
}

fn exact_test_cfg(node: Node<'_>, text: &str) -> Result<bool, String> {
    let mut cursor = node.walk();
    let Some(attribute) = node
        .named_children(&mut cursor)
        .find(|n| n.kind() == "attribute")
    else {
        return Err("malformed Rust attribute".into());
    };
    let Some(args) = attribute.child_by_field_name("arguments") else {
        return Ok(false);
    };
    let mut cursor = attribute.walk();
    let name = attribute
        .named_children(&mut cursor)
        .find(|n| !comment(*n) && n.id() != args.id());
    if !name.is_some_and(|n| n.kind() == "identifier" && &text[n.byte_range()] == "cfg") {
        return Ok(false);
    }
    let mut cursor = args.walk();
    let mut names = 0;
    for child in args.children(&mut cursor) {
        if comment(child) || matches!(child.kind(), "(" | ")") {
            continue;
        }
        if child.kind() != "identifier" || &text[child.byte_range()] != "test" {
            return Ok(false);
        }
        names += 1;
    }
    Ok(names == 1)
}

fn removable(kind: &str) -> bool {
    matches!(
        kind,
        "associated_type"
            | "const_item"
            | "enum_item"
            | "extern_crate_declaration"
            | "foreign_mod_item"
            | "function_item"
            | "function_signature_item"
            | "impl_item"
            | "macro_definition"
            | "mod_item"
            | "static_item"
            | "struct_item"
            | "trait_item"
            | "type_item"
            | "union_item"
            | "use_declaration"
    )
}

fn validate(root: Node<'_>) -> Result<(), String> {
    let mut stack = vec![(root, 0usize)];
    let mut nodes = 0usize;
    while let Some((node, depth)) = stack.pop() {
        nodes += 1;
        if nodes > MAX_NODES || depth > MAX_DEPTH {
            return Err("Rust AST exceeds node/depth bound".into());
        }
        let mut cursor = node.walk();
        stack.extend(
            node.named_children(&mut cursor)
                .map(|child| (child, depth + 1)),
        );
        if stack.len() > MAX_NODES {
            return Err("Rust AST exceeds node/depth bound".into());
        }
    }
    Ok(())
}

fn spans(root: Node<'_>, text: &str) -> Result<Vec<(usize, usize)>, String> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(parent) = pending.pop() {
        let mut cursor = parent.walk();
        let mut first = None;
        let mut gated = false;
        for child in parent.named_children(&mut cursor) {
            if comment(child) {
                if first.is_none() && doc(child, text) {
                    first = Some(child.start_byte());
                }
                continue;
            }
            if child.kind() == "inner_attribute_item" {
                if exact_test_cfg(child, text)? {
                    return Err("inner cfg(test) requires whole-module handling".into());
                }
                first = None;
                gated = false;
                continue;
            }
            if child.kind() == "attribute_item" {
                first.get_or_insert(child.start_byte());
                gated |= exact_test_cfg(child, text)?;
                continue;
            }
            if gated {
                if !removable(child.kind()) {
                    return Err("unsupported cfg(test) item shape".into());
                }
                result.push((first.ok_or("missing attribute span")?, child.end_byte()));
            } else {
                pending.push(child);
            }
            first = None;
            gated = false;
        }
        if gated {
            return Err("cfg(test) attribute has no item".into());
        }
    }
    result.sort_unstable();
    Ok(result)
}

pub fn production_source(text: &str) -> Result<String, String> {
    let tree = syntax::parse(text, Language::Rust).map_err(|e| format!("{e:#}"))?;
    validate(tree.root_node())?;
    let spans = spans(tree.root_node(), text)?;
    let mut result = String::with_capacity(text.len());
    let mut previous = 0;
    for (start, end) in spans {
        if start < previous || end < start {
            return Err("overlapping Rust item spans".into());
        }
        result.push_str(
            text.get(previous..start)
                .ok_or("invalid Rust source boundary")?,
        );
        previous = end;
    }
    result.push_str(text.get(previous..).ok_or("invalid Rust source boundary")?);
    Ok(result)
}

#[cfg(test)]
#[path = "candidate_rust_artifact_tests.rs"]
mod tests;
