//! Safe scalar constructors and implicit resolution, without creating objects.
use regex::Regex;
use std::sync::LazyLock;
use yaml_rust2::{parser::Tag as YamlTag, scanner::TScalarStyle};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tag {
    Null,
    Bool,
    Int,
    Float,
    Binary,
    Timestamp,
    Str,
    Seq,
    Map,
    Set,
    Omap,
    Pairs,
    Merge,
    Value,
}
static INT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[-+]?(?:0b[01_]+|0[0-7_]+|(?:0|[1-9][0-9_]*)|0x[0-9a-fA-F_]+|[1-9][0-9_]*(?::[0-5]?[0-9])+)$").expect("static YAML integer pattern")
});
static FLOAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[-+]?[0-9][0-9_]*\.[0-9_]*(?:[eE][-+][0-9]+)?|\.[0-9][0-9_]*(?:[eE][-+][0-9]+)?|[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*|[-+]?\.(?:inf|Inf|INF)|\.(?:nan|NaN|NAN))$").expect("static YAML float pattern")
});
static DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$").expect("static YAML date pattern")
});
static TIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<y>[0-9]{4})-(?P<m>[0-9]{1,2})-(?P<d>[0-9]{1,2})(?:[Tt]|[ \t]+)(?P<h>[0-9]{1,2}):(?P<min>[0-9]{2}):(?P<s>[0-9]{2})(?:\.[0-9]*)?(?:[ \t]*(?:Z|[-+](?P<zh>[0-9]{1,2})(?::(?P<zm>[0-9]{2}))?))?$").expect("static YAML timestamp pattern")
});
static EXPLICIT_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<y>[0-9]{4})-(?P<m>[0-9]{1,2})-(?P<d>[0-9]{1,2})$")
        .expect("static YAML explicit date pattern")
});
static ND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{Nd}$").expect("static decimal digit property"));
fn implicit(text: &str, style: TScalarStyle) -> Tag {
    if style != TScalarStyle::Plain {
        return Tag::Str;
    }
    if matches!(
        text,
        "yes"
            | "Yes"
            | "YES"
            | "no"
            | "No"
            | "NO"
            | "true"
            | "True"
            | "TRUE"
            | "false"
            | "False"
            | "FALSE"
            | "on"
            | "On"
            | "ON"
            | "off"
            | "Off"
            | "OFF"
    ) {
        return Tag::Bool;
    }
    if FLOAT.is_match(text) {
        return Tag::Float;
    }
    if INT.is_match(text) {
        return Tag::Int;
    }
    if matches!(text, "" | "~" | "null" | "Null" | "NULL") {
        return Tag::Null;
    }
    if DATE.is_match(text) || TIME.is_match(text) {
        return Tag::Timestamp;
    }
    match text {
        "<<" => Tag::Merge,
        "=" => Tag::Value,
        _ => Tag::Str,
    }
}
pub(super) fn resolve(
    tag: Option<YamlTag>,
    text: Option<(&str, TScalarStyle)>,
    default: Tag,
) -> Result<Tag, &'static str> {
    let Some(tag) = tag else {
        return Ok(text.map_or(default, |(text, style)| implicit(text, style)));
    };
    if tag.handle.is_empty() && tag.suffix == "!" {
        return Ok(default);
    }
    if tag.handle.len() + tag.suffix.len() > 128 {
        return Err("unknown YAML tag");
    }
    let name = format!("{}{}", tag.handle, tag.suffix);
    let name = name
        .strip_prefix("tag:yaml.org,2002:")
        .ok_or("unknown or unsafe YAML tag")?;
    Ok(match name {
        "null" => Tag::Null,
        "bool" => Tag::Bool,
        "int" => Tag::Int,
        "float" => Tag::Float,
        "binary" => Tag::Binary,
        "timestamp" => Tag::Timestamp,
        "str" => Tag::Str,
        "seq" => Tag::Seq,
        "map" => Tag::Map,
        "set" => Tag::Set,
        "omap" => Tag::Omap,
        "pairs" => Tag::Pairs,
        "merge" => Tag::Merge,
        "value" => Tag::Value,
        _ => return Err("unknown or unsafe YAML tag"),
    })
}
fn decimal(ch: char) -> Option<u8> {
    if ch.is_ascii_digit() {
        return Some(ch as u8 - b'0');
    }
    let mut bytes = [0; 4];
    if !ND.is_match(ch.encode_utf8(&mut bytes)) {
        return None;
    }
    let code = ch as u32;
    let mut start = code;
    // Unicode decimal blocks contain ten-digit sets (math styles are one 50-digit run).
    for _ in 0..64 {
        let Some(previous) = start.checked_sub(1).and_then(char::from_u32) else {
            break;
        };
        if !ND.is_match(previous.encode_utf8(&mut bytes)) {
            break;
        }
        start -= 1;
    }
    Some(((code - start) % 10) as u8)
}
fn numeric(text: &str) -> String {
    text.chars()
        .map(|ch| decimal(ch).map_or(ch, |digit| char::from(b'0' + digit)))
        .collect()
}
fn digits(text: &str, radix: u32, prefix: &str) -> bool {
    let text = numeric(text);
    let text = text.trim();
    let text = text.strip_prefix(['+', '-']).unwrap_or(text);
    let text = if prefix.is_empty() {
        text
    } else {
        text.strip_prefix(prefix).unwrap_or(text)
    };
    !text.is_empty() && text.chars().all(|ch| ch.is_ascii() && ch.is_digit(radix))
}
fn integer(text: &str) -> bool {
    let text = text.replace('_', "");
    if text.is_empty() {
        return false;
    }
    let text = text.strip_prefix(['+', '-']).unwrap_or(&text);
    if text == "0" {
        true
    } else if let Some(text) = text.strip_prefix("0b") {
        digits(text, 2, "0b")
    } else if let Some(text) = text.strip_prefix("0x") {
        digits(text, 16, "0x")
    } else if text.starts_with('0') {
        digits(text, 8, "0o")
    } else if text.contains(':') {
        text.split(':').all(|part| digits(part, 10, ""))
    } else {
        digits(text, 10, "")
    }
}
fn float(text: &str) -> bool {
    let text = numeric(&text.replace('_', "").to_ascii_lowercase());
    let text = text.strip_prefix(['+', '-']).unwrap_or(&text);
    matches!(text, ".inf" | ".nan")
        || if text.contains(':') {
            text.split(':')
                .all(|part| part.trim().parse::<f64>().is_ok())
        } else {
            text.trim().parse::<f64>().is_ok()
        }
}
fn binary(text: &str) -> bool {
    if !text.is_ascii() {
        return false;
    }
    let mut position = 0usize;
    let mut pads = 0;
    for byte in text.bytes() {
        if byte == b'=' {
            if position >= 2 {
                pads += 1;
                if position + pads >= 4 {
                    return true;
                }
            }
        } else if byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/') {
            position = (position + 1) % 4;
            pads = 0;
        }
    }
    position == 0
}
fn timestamp(text: &str) -> bool {
    // Python's constructor regex '$' also matches immediately before one final LF.
    let text = text.strip_suffix('\n').unwrap_or(text);
    let captures = TIME.captures(text).or_else(|| EXPLICIT_DATE.captures(text));
    let Some(c) = captures else {
        return false;
    };
    let number = |key| c.name(key).and_then(|m| m.as_str().parse::<u32>().ok());
    let (Some(y), Some(m), Some(d)) = (number("y"), number("m"), number("d")) else {
        return false;
    };
    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return false;
    }
    if crate::civil::civil_from_days(crate::civil::days_from_civil(y as i64, m, d))
        != (y as i64, m, d)
    {
        return false;
    }
    if let Some(hour) = number("h") {
        if hour > 23 || number("min").is_none_or(|n| n > 59) || number("s").is_none_or(|n| n > 59) {
            return false;
        }
    }
    // datetime.timezone normalizes minute overflow but requires abs(offset)<24h.
    number("zh").is_none_or(|hours| hours * 60 + number("zm").unwrap_or(0) < 24 * 60)
}
pub(super) fn validate(tag: Tag, text: &str) -> Result<(), &'static str> {
    let valid = match tag {
        Tag::Null | Tag::Str => true,
        Tag::Bool => matches!(
            text.to_ascii_lowercase().as_str(),
            "yes" | "no" | "true" | "false" | "on" | "off"
        ),
        Tag::Int => integer(text),
        Tag::Float => float(text),
        Tag::Binary => binary(text),
        Tag::Timestamp => timestamp(text),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err("invalid YAML safe scalar constructor")
    }
}
