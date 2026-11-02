//! Iterative lexical and syntax-depth admission before recursive `syn` parsing.
//! Strings and comments are skipped, including nested Rust block comments.
use crate::syntax::{self, Language};
const MAX_GROUP_DEPTH: usize = 64;
const MAX_OPERATORS: usize = 128;
const MAX_NODES: usize = 200_000;
const MAX_TREE_DEPTH: usize = 64;
const MAX_LEX_WORK: usize = 32 * 1024 * 1024;

pub(super) fn admit(source: &str) -> Result<(), String> {
    if source.len() > syntax::MAX_SOURCE_BYTES {
        return Err("Rust source exceeds 1 MiB budget".into());
    }
    lexical(source)?;
    let tree = syntax::parse(source, Language::Rust).map_err(|e| e.to_string())?;
    let mut stack = vec![(tree.root_node(), 0usize)];
    let mut visited = 0usize;
    while let Some((node, depth)) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES || depth > MAX_TREE_DEPTH {
            return Err("Rust syntax tree exceeds node/depth budget before syn".into());
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if depth + 1 > MAX_TREE_DEPTH || visited + stack.len() >= MAX_NODES {
                return Err("Rust syntax tree exceeds node/depth budget before syn".into());
            }
            stack.push((child, depth + 1));
        }
    }
    Ok(())
}
fn lexical(source: &str) -> Result<(), String> {
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut groups = Vec::new();
    let mut operators = 0usize;
    let mut work = 0usize;
    while i < bytes.len() {
        work = work.checked_add(1).ok_or("Rust lexical work overflow")?;
        if work > MAX_LEX_WORK {
            return Err("Rust lexical work budget exceeded".into());
        }
        if bytes[i..].starts_with(b"//") {
            i = line_comment(bytes, i + 2);
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            i = block_comment(bytes, i + 2)?;
            continue;
        }
        if bytes[i] == b'r' {
            if let Some(end) = raw_string(bytes, i, &mut work)? {
                i = end;
                continue;
            }
        }
        if bytes[i] == b'"' {
            i = quoted(bytes, i)?;
            continue;
        }
        if bytes[i] == b'\'' {
            if let Some(end) = character(source, i) {
                i = end;
                continue;
            }
        }
        match bytes[i] {
            b'(' | b'[' | b'{' => {
                groups.push(bytes[i]);
                if groups.len() > MAX_GROUP_DEPTH {
                    return Err("Rust delimiter depth budget exceeded before syn".into());
                }
                if bytes[i] == b'{' {
                    operators = 0;
                }
            }
            b')' | b']' | b'}' => {
                let expected = match bytes[i] {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if groups.pop() != Some(expected) {
                    return Err("unbalanced Rust delimiters".into());
                }
                if bytes[i] == b'}' {
                    operators = 0;
                }
            }
            b';' | b',' => operators = 0,
            b'!' | b'*' | b'&' | b'-' | b'+' | b'<' | b'>' | b'|' | b'^' | b'=' | b'?' | b'/'
            | b'%' => {
                operators += 1;
                if operators > MAX_OPERATORS {
                    return Err("Rust operator nesting budget exceeded before syn".into());
                }
            }
            _ => (),
        }
        i += 1;
    }
    if !groups.is_empty() {
        return Err("unclosed Rust delimiters".into());
    }
    Ok(())
}
fn line_comment(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}
fn block_comment(bytes: &[u8], mut i: usize) -> Result<usize, String> {
    let mut depth = 1usize;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"/*") {
            depth += 1;
            if depth > MAX_GROUP_DEPTH {
                return Err("Rust comment depth budget exceeded".into());
            }
            i += 2;
        } else if bytes[i..].starts_with(b"*/") {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return Ok(i);
            }
        } else {
            i += 1;
        }
    }
    Err("unterminated Rust block comment".into())
}
fn quoted(bytes: &[u8], start: usize) -> Result<usize, String> {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 1;
                if i < bytes.len() {
                    i += 1;
                }
            }
            b'"' => return Ok(i + 1),
            _ => i += 1,
        }
    }
    Err("unterminated Rust string literal".into())
}
fn raw_string(bytes: &[u8], start: usize, work: &mut usize) -> Result<Option<usize>, String> {
    let mut i = start + 1;
    while i < bytes.len() && bytes[i] == b'#' {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'"' {
        return Ok(None);
    }
    let count = i - start - 1;
    if count > 255 {
        return Err("Rust raw string delimiter exceeds 255 hashes".into());
    }
    i += 1;
    while i < bytes.len() {
        if bytes[i] == b'"' && bytes.len() - i > count {
            *work = work
                .checked_add(count)
                .ok_or("Rust lexical work overflow")?;
            if *work > MAX_LEX_WORK {
                return Err("Rust lexical work budget exceeded".into());
            }
            if bytes[i + 1..i + 1 + count].iter().all(|b| *b == b'#') {
                return Ok(Some(i + 1 + count));
            }
        }
        i += 1;
    }
    Err("unterminated Rust raw string literal".into())
}
fn character(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = start + 1;
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b'\\' {
        i += 1;
        let escape = *bytes.get(i)?;
        i += 1;
        if escape == b'x' {
            i = i.checked_add(2)?;
        } else if escape == b'u' && *bytes.get(i)? == b'{' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'}' {
                i += 1;
            }
            i += 1;
        }
    } else {
        let ch = source.get(i..)?.chars().next()?;
        i = i.checked_add(ch.len_utf8())?;
    }
    if bytes.get(i) == Some(&b'\'') {
        Some(i + 1)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn group_unary_binary_generic_and_nested_comments_are_refused() {
        for source in [
            format!(
                "fn f() {{ let _ = {}0{}; }}",
                "(".repeat(600),
                ")".repeat(600)
            ),
            format!("fn f() {{ let _ = {}true; }}", "!".repeat(600)),
            format!("fn f() {{ let _ = {}0; }}", "0+".repeat(600)),
            format!("type T = {}u8{};", "Vec<".repeat(200), ">".repeat(200)),
            format!("{} x {} fn f() {{}}", "/*".repeat(100), "*/".repeat(100)),
        ] {
            assert!(admit(&source).is_err(), "source unexpectedly admitted");
        }
    }
    #[test]
    fn tree_depth_catches_binary_chains_below_lexical_operator_limit() {
        let source = format!("fn f() {{ let _ = {}0; }}", "0+".repeat(75));
        assert!(lexical(&source).is_ok());
        assert!(admit(&source).unwrap_err().contains("depth"));
    }
    #[test]
    fn delimiters_and_operators_in_literals_comments_and_lifetimes_are_not_code() {
        let data = "(".repeat(700);
        let ops = "!+".repeat(300);
        let source = format!(
            r###"// {data} {ops}
/* outer /* {data} */ still outer */
#[test] fn check<'a>(x: &'a str) {{
    let _ = r##"{data} {ops}"##;
    let _ = br#"{data}"#;
    let _ = "{data}";
    let _ = '('; let _ = '\''; let _ = x;
}}"###
        );
        assert!(admit(&source).is_ok());
    }
    #[test]
    fn malformed_lexical_state_and_unbalanced_groups_fail_closed() {
        for source in [
            "fn f() { /*",
            "fn f() { let _ = r#\"unterminated;",
            "fn f() { let _ = (1]; }",
        ] {
            assert!(admit(source).is_err());
        }
    }
}
