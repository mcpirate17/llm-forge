use super::{Inspection, Language};
use crate::graph_index::{Call, Definition, FileFacts};
use anyhow::{ensure, Result};

pub(super) fn fold(inspection: &Inspection, source: &str) -> Result<String> {
    ensure!(
        super::fingerprint(source) == inspection.source_sha256,
        "source changed since syntax inspection; refusing stale body offsets"
    );
    let mut ranges: Vec<_> = inspection
        .symbols
        .iter()
        .filter(|s| s.kind == "Function")
        .filter_map(|s| s.body)
        .collect();
    ranges.sort_by_key(|&(start, end)| (start, std::cmp::Reverse(end)));
    let mut output = String::with_capacity(source.len());
    let mut offset = 0;
    for (start, end) in ranges {
        ensure!(
            start < end
                && end <= source.len()
                && source.is_char_boundary(start)
                && source.is_char_boundary(end),
            "invalid body span in syntax projection"
        );
        if start < offset {
            continue;
        }
        output.push_str(&source[offset..start]);
        output.push_str(if inspection.language == Language::Python {
            "pass  # body omitted"
        } else if source.as_bytes()[start] == b'{' {
            "{ /* body omitted */ }"
        } else {
            "undefined /* body omitted */"
        });
        offset = end;
    }
    output.push_str(&source[offset..]);
    Ok(output)
}

pub(super) fn facts(inspection: &Inspection) -> FileFacts {
    FileFacts {
        definitions: inspection
            .symbols
            .iter()
            .map(|s| Definition {
                qualified: s.qualified.clone(),
                name: s.name.clone(),
                kind: s.kind.clone(),
                line_start: s.line_start,
                line_end: s.line_end,
                signature: graph_signature(&s.signature),
            })
            .collect(),
        calls: inspection
            .references
            .iter()
            .filter(|r| r.role == "call")
            .map(|r| Call {
                caller: r.scope.clone(),
                target: r.name.clone(),
                line: r.line,
            })
            .collect(),
        dynamic_calls: inspection.dynamic_calls,
        path_imports: inspection
            .dependencies
            .iter()
            .map(|dependency| dependency.target.clone())
            .collect(),
        cross_file_incomplete: true,
        ..FileFacts::default()
    }
}

fn graph_signature(signature: &str) -> String {
    let mut end = signature.len().min(512);
    while !signature.is_char_boundary(end) {
        end -= 1;
    }
    signature[..end].to_owned()
}
