//! Bounded, checked source-to-source ablation proposals. No source is written here.
use crate::engine::{self, Ablation, Rule};
use crate::rules;
use std::collections::HashSet;

pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;

pub fn select(
    names: Option<&[String]>,
    extra: &[String],
) -> Result<Vec<(&'static str, Rule)>, String> {
    let registry: Vec<_> = rules::DEFAULT
        .iter()
        .chain(rules::OPTIONAL)
        .copied()
        .collect();
    let mut chosen = match names {
        None => rules::DEFAULT.to_vec(),
        Some(names) => names
            .iter()
            .map(|name| {
                registry
                    .iter()
                    .find(|(key, _)| key == name)
                    .copied()
                    .ok_or_else(|| format!("unknown ablation rule: {name}"))
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    for name in extra {
        chosen.push(
            rules::OPTIONAL
                .iter()
                .find(|(key, _)| key == name)
                .copied()
                .ok_or_else(|| format!("unknown optional ablation rule: {name}"))?,
        );
    }
    let mut seen = HashSet::new();
    chosen.retain(|(name, _)| seen.insert(*name));
    Ok(chosen)
}

fn checked_tree(source: &str) -> Result<crate::source::PythonSource<'_>, String> {
    let parsed = crate::source::PythonSource::new(source)?;
    let tree = parsed.tree();
    let mut stack = vec![tree.root_node()];
    let mut reference_work = 0usize;
    while let Some(node) = stack.pop() {
        if node.kind() == "function_definition" {
            let parameters = node
                .child_by_field_name("parameters")
                .map_or(0, |p| p.named_child_count());
            reference_work =
                reference_work.saturating_add(parameters.saturating_mul(node.descendant_count()));
            if parameters > 64 || reference_work > 1_000_000 {
                return Err("ablation parameter/reference work budget exceeded".into());
            }
        }
        if matches!(node.kind(), "default_parameter" | "typed_default_parameter")
            && node
                .child_by_field_name("value")
                .is_some_and(|v| v.end_byte() - v.start_byte() > 4096)
        {
            return Err("ablation default expression exceeds 4096 bytes".into());
        }
        stack.extend(engine::named(node));
    }
    Ok(parsed)
}

pub fn collect(source: &str, rules: &[(&str, Rule)]) -> Result<Vec<Ablation>, String> {
    let tree = checked_tree(source)?;
    engine::collect_tree(source, tree.tree(), rules)
}

/// Reject invalid, overlapping, ambiguous or non-UTF-8 edit boundaries before rewriting.
/// The resulting source must parse, and the caller receives it without any file mutation.
pub fn apply(source: &str, ablation: &Ablation) -> Result<String, String> {
    checked_tree(source)?;
    let mut edits: Vec<_> = ablation.edits.iter().collect();
    edits.sort_by_key(|e| (e.start, e.end));
    let mut previous = None;
    let mut length = source.len();
    for edit in &edits {
        if edit.start > edit.end
            || !source.is_char_boundary(edit.start)
            || !source.is_char_boundary(edit.end)
        {
            return Err("invalid ablation edit range".into());
        }
        if previous.is_some_and(|(start, end)| edit.start < end || edit.start == start) {
            return Err("overlapping or ambiguous ablation edits".into());
        }
        previous = Some((edit.start, edit.end));
        length = length
            .checked_sub(edit.end - edit.start)
            .and_then(|n| n.checked_add(edit.replacement.len()))
            .ok_or("ablation length overflow")?;
    }
    if length > MAX_SOURCE_BYTES {
        return Err("ablated source exceeds 1 MiB".into());
    }
    let mut out = String::with_capacity(length);
    let mut cursor = 0;
    for edit in edits {
        out.push_str(&source[cursor..edit.start]);
        out.push_str(&edit.replacement);
        cursor = edit.end;
    }
    out.push_str(&source[cursor..]);
    checked_tree(&out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Edit;

    #[test]
    fn selectors_are_explicit_and_stable_without_duplicate_proposals() {
        assert!(select(Some(&["unknown".into()]), &[]).is_err());
        assert!(select(None, &["drop_clamp".into()]).is_err());
        assert_eq!(
            select(Some(&["drop_clamp".into(), "drop_clamp".into()]), &[])
                .unwrap()
                .len(),
            1
        );
        assert!(select(Some(&[]), &[]).unwrap().is_empty());
    }

    #[test]
    fn invalid_sources_and_unbounded_trees_are_errors() {
        let rules = select(None, &[]).unwrap();
        assert!(collect("def broken(:", &rules).is_err());
        assert!(collect("1.0 = value\n", &rules).is_err());
        assert!(collect(&"#".repeat(MAX_SOURCE_BYTES + 1), &rules).is_err());
        let source = format!("x={}1{}\n", "(".repeat(140), ")".repeat(140));
        assert!(collect(&source, &rules).is_err());
    }

    #[test]
    fn proposal_and_reference_expansion_are_bounded_before_large_allocations() {
        let rules = select(None, &[]).unwrap();
        let source = format!("def f(x):\n{}", "    x.detach()\n".repeat(4097));
        assert!(collect(&source, &rules)
            .unwrap_err()
            .contains("proposal budget"));
        let source = format!("def f(x='{}'):\n    return x\n", "s".repeat(4097));
        assert!(collect(&source, &rules)
            .unwrap_err()
            .contains("default expression"));
        let params = (0..65)
            .map(|i| format!("x{i}=1"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            collect(&format!("def f({params}):\n    return x0\n"), &rules)
                .unwrap_err()
                .contains("reference work")
        );
    }

    #[test]
    fn edit_bounds_overlap_and_syntax_fail_without_panics() {
        let source = "x = 'é'\n";
        let mut ablation = Ablation {
            rule: "fixture".into(),
            qualname: "".into(),
            line: 1,
            description: "".into(),
            edits: vec![],
        };
        for (start, end, replacement) in [(6, 7, "a"), (0, 99, ""), (3, 2, ""), (0, 1, "(")] {
            ablation.edits = vec![Edit {
                start,
                end,
                replacement: replacement.into(),
            }];
            assert!(apply(source, &ablation).is_err());
        }
        ablation.edits = vec![
            Edit {
                start: 0,
                end: 1,
                replacement: "y".into()
            };
            2
        ];
        assert!(apply(source, &ablation).is_err());
        ablation.edits.pop();
        assert_eq!(apply(source, &ablation).unwrap(), "y = 'é'\n");
    }
}
