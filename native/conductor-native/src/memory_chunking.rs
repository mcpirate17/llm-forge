//! Deterministic markdown chunking for the workspace memory index.
//!
//! The Python boundary keeps catalog walking, embedding requests and index
//! writes. Rust owns the line-loop chunker that ran as a Python closure over
//! every line of every indexed file.

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
#[pyfunction]
#[pyo3(signature = (text, source_id, path, title, mode))]
fn memory_index_chunk_text_native(
    text: &str,
    source_id: &str,
    path: &str,
    title: &str,
    mode: &str,
) -> PyResult<Vec<(String, String, String, String)>> {
    let text = py_strip(text);
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let chunks = if mode == "whole" {
        vec![(
            source_id.to_owned(),
            path.to_owned(),
            title.to_owned(),
            take_chars(text, WHOLE_MODE_LIMIT),
        )]
    } else {
        markdown_chunks(text, source_id, path, title)
    };
    Ok(chunks)
}

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
            memory_index_chunk_text_native("   \n  ", "s", "p", "t", "lines").unwrap(),
            Vec::<(String, String, String, String)>::new()
        );
    }

    #[test]
    fn whole_mode_truncates_at_3000_chars() {
        let text = "x".repeat(4000);
        let out = memory_index_chunk_text_native(&text, "s", "p", "t", "whole").unwrap();
        assert_eq!(out[0].3.chars().count(), 3000);
    }
}
