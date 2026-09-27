//! Shared A2A whitespace normalization for message receipts and inbox previews.

// Python's regular-expression `\s` includes the four ASCII information
// separators in addition to Unicode White_Space. Keep that exact behavior.
pub(crate) fn python_whitespace(value: char) -> bool {
    value.is_whitespace() || ('\u{001c}'..='\u{001f}').contains(&value)
}

/// Collapse Python whitespace runs to one ASCII space and strip their edges.
pub fn normalized_text(value: &str) -> String {
    value
        .split(python_whitespace)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::normalized_text;

    #[test]
    fn ascii_information_separators_are_python_whitespace() {
        assert_eq!(
            normalized_text("\u{001c}alpha\u{001d}beta\u{001e}gamma\u{001f}delta\u{001c}"),
            "alpha beta gamma delta"
        );
    }

    #[test]
    fn unicode_and_standard_whitespace_collapse_without_losing_text() {
        assert_eq!(
            normalized_text(" \t\r\n café\u{00a0}\u{2003}東京\u{2028}🦀 \t"),
            "café 東京 🦀"
        );
        assert_eq!(normalized_text("\u{001f}\u{3000}\n"), "");
        assert_eq!(normalized_text(""), "");
    }

    #[test]
    fn non_whitespace_controls_and_format_characters_are_preserved() {
        assert_eq!(
            normalized_text("a\u{0000}b\u{200b}c\u{feff}d"),
            "a\u{0000}b\u{200b}c\u{feff}d"
        );
    }
}
