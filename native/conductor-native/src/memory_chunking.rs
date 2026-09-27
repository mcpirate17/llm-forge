//! Deterministic markdown chunking for the workspace memory index.
//!
//! The Python boundary keeps catalog walking, embedding requests and index
//! writes. Rust owns the line-loop chunker that ran as a Python closure over
//! every line of every indexed file.

#[cfg(feature = "python")]
use pyo3::prelude::*;

const MAX_CHUNK_CHARS: usize = 1500;
const HEADING_FLUSH_SIZE: usize = 400;
const WHOLE_MODE_LIMIT: usize = 1500 * 2;

fn is_py_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

/// Python ``str.splitlines()`` boundary set, terminator dropped.
fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0_usize;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let is_boundary = matches!(
            ch,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'
                ..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if is_boundary {
            lines.push(&text[start..index]);
            if ch == '\r' && matches!(chars.peek(), Some((_, '\n'))) {
                chars.next();
            }
            start = chars.peek().map_or(text.len(), |(next, _)| *next);
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn py_strip(text: &str) -> &str {
    text.trim_matches(is_py_space)
}

fn take_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// Mirror ``memory_index.chunk_text`` for non-whole mode.
fn markdown_chunks(
    text: &str,
    source_id: &str,
    path: &str,
    fallback_title: &str,
) -> Vec<(String, String, String, String)> {
    let mut chunks: Vec<(String, String, String, String)> = Vec::new();
    let mut buf: Vec<&str> = Vec::new();
    let mut title = fallback_title;
    let mut size = 0_usize;
    let flush = |chunks: &mut Vec<(String, String, String, String)>,
                 buf: &mut Vec<&str>,
                 size: &mut usize,
                 title: &str| {
        let joined = buf.join("\n");
        let body = py_strip(&joined);
        if !body.is_empty() {
            chunks.push((
                source_id.to_owned(),
                path.to_owned(),
                title.to_owned(),
                take_chars(body, WHOLE_MODE_LIMIT),
            ));
        }
        buf.clear();
        *size = 0;
    };
    for line in splitlines(text) {
        let is_heading = ["# ", "## ", "### ", "#### "]
            .iter()
            .any(|prefix| line.starts_with(prefix));
        if is_heading && size >= HEADING_FLUSH_SIZE {
            flush(&mut chunks, &mut buf, &mut size, title);
            let stripped = line.trim_start_matches('#');
            let new_title = py_strip(stripped);
            title = if new_title.is_empty() {
                fallback_title
            } else {
                new_title
            };
        }
        buf.push(line);
        size += line.chars().count() + 1;
        if size >= MAX_CHUNK_CHARS {
            flush(&mut chunks, &mut buf, &mut size, title);
        }
    }
    flush(&mut chunks, &mut buf, &mut size, title);
    chunks
}

/// Native entry: ``memory_index.chunk_text``. Returns a JSON array of chunk
/// objects, parsed once on the Python side.
pub fn chunk_text(
    text: &str,
    source_id: &str,
    path: &str,
    title: &str,
    mode: &str,
) -> Vec<(String, String, String, String)> {
    let text = py_strip(text);
    if text.is_empty() {
        return Vec::new();
    }
    if mode == "whole" {
        vec![(
            source_id.to_owned(),
            path.to_owned(),
            title.to_owned(),
            take_chars(text, WHOLE_MODE_LIMIT),
        )]
    } else {
        markdown_chunks(text, source_id, path, title)
    }
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (text, source_id, path, title, mode))]
fn memory_index_chunk_text_native(
    text: &str,
    source_id: &str,
    path: &str,
    title: &str,
    mode: &str,
) -> PyResult<Vec<(String, String, String, String)>> {
    Ok(chunk_text(text, source_id, path, title, mode))
}

#[cfg(feature = "python")]
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(memory_index_chunk_text_native, module)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_matches_python_boundaries() {
        assert_eq!(
            splitlines("a\nb\r\nc\rd\x0be\u{85}f\u{2028}g\u{2029}h\x1ci"),
            vec!["a", "b", "c", "d", "e", "f", "g", "h", "i"]
        );
        assert_eq!(splitlines("a\n"), vec!["a"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
    }

    #[test]
    fn empty_text_yields_no_chunks() {
        assert_eq!(
            chunk_text("   \n  ", "s", "p", "t", "lines"),
            Vec::<(String, String, String, String)>::new()
        );
    }

    #[test]
    fn whole_mode_truncates_at_3000_chars() {
        let text = "x".repeat(4000);
        let out = chunk_text(&text, "s", "p", "t", "whole");
        assert_eq!(out[0].3.chars().count(), 3000);
    }

    #[test]
    fn heading_after_four_hundred_characters_starts_a_new_chunk() {
        let text = format!("# One\n{}\n## Two\n{}", "a".repeat(500), "b".repeat(500));
        let out = chunk_text(&text, "notes", "x.md", "x.md", "heading");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "notes");
        assert_eq!(out[0].2, "x.md");
        assert!(out[0].3.starts_with("# One\n"));
        assert_eq!(out[1].2, "Two");
        assert!(out[1].3.starts_with("## Two\n"));
    }

    #[test]
    fn heading_only_uses_fallback_title_and_preserves_path() {
        let out = chunk_text("# ###", "s", "docs/n.md", "n.md", "chunk");
        assert_eq!(
            out,
            vec![(
                "s".to_owned(),
                "docs/n.md".to_owned(),
                "n.md".to_owned(),
                "# ###".to_owned(),
            )]
        );
    }

    #[test]
    fn chunk_mode_splits_at_character_limit_without_breaking_utf8() {
        let text = "雪".repeat(1_500);
        let out = chunk_text(&text, "s", "docs/n.md", "n.md", "chunk");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].3.chars().count(), 1_500);
        assert_eq!(out[0].3, text);
    }
}
