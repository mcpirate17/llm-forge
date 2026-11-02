//! Recover CPython lone-surrogate values that Ruff represents as U+FFFD.
//! Same-length escape rewrites preserve source offsets and never touch raw/bytes tokens.
use super::*;
use ast::StringFlags;
use ruff_text_size::Ranged;
use std::collections::{BTreeMap, BTreeSet};

pub(super) type Markers = BTreeMap<u32, u32>;
pub(super) fn sanitize(
    source: &str,
    tokens: &ast::token::Tokens,
) -> Result<(String, Markers), String> {
    let mut bytes = source.as_bytes().to_vec();
    let mut used = used_private(source);
    let mut by_surrogate = BTreeMap::new();
    let mut markers = Markers::new();
    let mut next = 0xe000u32;
    for token in tokens {
        if !matches!(
            token.kind(),
            ast::token::TokenKind::String | ast::token::TokenKind::FStringMiddle
        ) {
            continue;
        }
        let Some(flags) = token.string_flags() else {
            continue;
        };
        let prefix = flags.prefix();
        if prefix.is_raw() || matches!(prefix, ast::str_prefix::AnyStringPrefix::Bytes(_)) {
            continue;
        }
        let range = token.range();
        let mut i = range.start().to_usize();
        let end = range.end().to_usize();
        while i < end {
            if bytes[i] != b'\\' {
                i += 1;
                continue;
            }
            if i + 1 >= end {
                break;
            }
            let (hex, n) = match bytes[i + 1] {
                b'u' => (i + 2, 4),
                b'U' => (i + 2, 8),
                _ => {
                    i += 2;
                    continue;
                }
            };
            if hex + n > end {
                i += 2;
                continue;
            }
            let digits = std::str::from_utf8(&bytes[hex..hex + n]).map_err(|e| e.to_string())?;
            let Ok(value) = u32::from_str_radix(digits, 16) else {
                i += 2;
                continue;
            };
            if (0xd800..=0xdfff).contains(&value) {
                let marker = match by_surrogate.get(&value) {
                    Some(marker) => *marker,
                    None => {
                        while next <= 0xf8ff && used.contains(&next) {
                            next += 1;
                        }
                        if next > 0xf8ff {
                            return Err("surrogate marker space exhausted".into());
                        }
                        let marker = next;
                        next += 1;
                        used.insert(marker);
                        by_surrogate.insert(value, marker);
                        markers.insert(marker, value);
                        marker
                    }
                };
                let replacement = format!("{marker:0width$x}", width = n);
                bytes[hex..hex + n].copy_from_slice(replacement.as_bytes());
            }
            i = hex + n;
        }
    }
    Ok((
        String::from_utf8(bytes).map_err(|e| e.to_string())?,
        markers,
    ))
}
fn used_private(source: &str) -> BTreeSet<u32> {
    let mut used: BTreeSet<u32> = source
        .chars()
        .map(u32::from)
        .filter(|v| (0xe000..=0xf8ff).contains(v))
        .collect();
    let bytes = source.as_bytes();
    for (prefix, n) in [(b'u', 4usize), (b'U', 8usize)] {
        for w in bytes.windows(n + 2) {
            if w[0] != b'\\' || w[1] != prefix {
                continue;
            }
            if let Ok(digits) = std::str::from_utf8(&w[2..]) {
                if let Ok(value) = u32::from_str_radix(digits, 16) {
                    if (0xe000..=0xf8ff).contains(&value) {
                        used.insert(value);
                    }
                }
            }
        }
    }
    used
}
pub(super) fn restore_repr(text: &str, markers: &Markers) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i] == b'\\' {
            i += 1;
        }
        let count = i - start;
        out.extend_from_slice(&bytes[start..i]);
        if count % 2 == 1 && i + 5 <= bytes.len() && bytes[i] == b'u' {
            if let Ok(digits) = std::str::from_utf8(&bytes[i + 1..i + 5]) {
                if let Ok(marker) = u32::from_str_radix(digits, 16) {
                    if let Some(value) = markers.get(&marker) {
                        out.extend_from_slice(format!("u{value:04x}").as_bytes());
                        i += 5;
                    }
                }
            }
        }
    }
    String::from_utf8(out).expect("rewriting ASCII escapes preserves UTF-8")
}
