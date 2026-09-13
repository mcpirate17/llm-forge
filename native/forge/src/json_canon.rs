//! Canonical JSON serialization: object keys sorted, compact separators,
//! UTF-8 preserved -- byte-for-byte Python's
//! `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)`.
//!
//! One form, one definition, because two frozen byte formats depend on it:
//! governance claim ids (`ownership::sha256_json` digests exactly these
//! bytes) and ledger day-file rows (`ledger::writer` writes exactly these
//! bytes, pinned by the hand-computed fixtures in `tests/ledger_rollup.rs`).
//! Both predate serde_json's `preserve_order` feature, under which
//! `to_string` emits insertion order instead of sorted -- so neither may
//! depend on that global cargo feature (enabled for `hooks_install`, which
//! must preserve a host settings file's own key order).

use serde_json::Value;

/// Serializes `value` with recursively sorted object keys and compact
/// separators. Arrays keep their order (Python `sort_keys` sorts keys, not
/// array elements); scalars serialize exactly as `serde_json::to_string`
/// renders them.
pub(crate) fn canonical_json(value: &Value) -> String {
    fn write(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
                keys.sort_unstable();
                out.push('{');
                for (n, key) in keys.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).expect("key serializes"));
                    out.push(':');
                    write(&map[*key], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (n, item) in items.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&serde_json::to_string(other).expect("Value serializes")),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The property everything here hinges on: the bytes must not depend on
    /// map insertion order (`preserve_order` changes it, the frozen formats
    /// must not).
    #[test]
    fn key_order_is_canonical_not_insertion_order() {
        let inserted = json!({"z": 1, "a": {"y": [2, {"b": 3, "a": 4}], "x": 5}});
        let mut shuffled = serde_json::Map::new();
        shuffled.insert("a".into(), json!({"x": 5, "y": [2, {"a": 4, "b": 3}]}));
        shuffled.insert("z".into(), json!(1));
        assert_eq!(
            canonical_json(&inserted),
            canonical_json(&Value::Object(shuffled))
        );
    }

    #[test]
    fn matches_python_dumps_sorted_compact_form() {
        assert_eq!(
            canonical_json(&json!({"b": 1, "a": "s"})),
            r#"{"a":"s","b":1}"#
        );
        // Arrays keep order, nesting is sorted at every level.
        assert_eq!(
            canonical_json(&json!([{"b": 1, "a": 2}, null, true])),
            r#"[{"a":2,"b":1},null,true]"#
        );
        assert_eq!(canonical_json(&json!({})), "{}");
        assert_eq!(canonical_json(&Value::Null), "null");
        // Non-ASCII stays raw UTF-8, like ensure_ascii=False.
        assert_eq!(canonical_json(&json!("é")), "\"é\"");
    }
}
