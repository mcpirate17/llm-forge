//! Executable test double for `python -m tooling.hooks.dispatch <event>`.
//!
//! `hook_delegation.rs` installs this binary as a project's `.venv/bin/python`.
//! Rust owns the fixture behavior; the production Forge binary does not build it.

use serde_json::{json, Value};
use std::env;
use std::io::{self, Read, Write};

fn escaped_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{007f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c if (c as u32) < 0x80 => out.push(c),
            c if (c as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => {
                let code = c as u32 - 0x10000;
                out.push_str(&format!(
                    "\\u{:04x}\\u{:04x}",
                    0xd800 + (code >> 10),
                    0xdc00 + (code & 0x3ff)
                ));
            }
        }
    }
    out.push('"');
    out
}

fn python_number(raw: &str) -> String {
    if !raw.contains(['.', 'e', 'E']) {
        return if raw == "-0" {
            "0".to_owned()
        } else {
            raw.to_owned()
        };
    }
    let number: f64 = raw.parse().expect("parsed JSON number");
    if number.is_infinite() {
        return if number.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        }
        .to_owned();
    }
    if number == 0.0 {
        return if number.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_owned();
    }
    let shortest = serde_json::to_string(&number).expect("finite float serializes");
    let (sign, unsigned) = shortest
        .strip_prefix('-')
        .map_or(("", shortest.as_str()), |rest| ("-", rest));
    let (mantissa, exponent) = unsigned
        .split_once(['e', 'E'])
        .map_or((unsigned, 0), |(mantissa, exponent)| {
            (mantissa, exponent.parse::<i32>().unwrap())
        });
    let point = mantissa.find('.').unwrap_or(mantissa.len());
    let digits: String = mantissa.chars().filter(|ch| *ch != '.').collect();
    let first = digits.find(|ch| ch != '0').unwrap();
    let mut significant = digits[first..].to_owned();
    while significant.len() > 1 && significant.ends_with('0') {
        significant.pop();
    }
    let power = exponent + point as i32 - first as i32 - 1;
    if (-4..16).contains(&power) {
        let decimal = power + 1;
        if decimal <= 0 {
            format!("{sign}0.{}{}", "0".repeat((-decimal) as usize), significant)
        } else if decimal as usize >= significant.len() {
            format!(
                "{sign}{}{}.0",
                significant,
                "0".repeat(decimal as usize - significant.len())
            )
        } else {
            format!(
                "{sign}{}.{}",
                &significant[..decimal as usize],
                &significant[decimal as usize..]
            )
        }
    } else {
        let coefficient = if significant.len() == 1 {
            significant
        } else {
            format!("{}.{}", &significant[..1], &significant[1..])
        };
        format!(
            "{sign}{coefficient}e{}{:02}",
            if power < 0 { '-' } else { '+' },
            power.abs()
        )
    }
}

fn python_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn python_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(true) => "true".to_owned(),
        Value::Bool(false) => "false".to_owned(),
        Value::Number(number) => python_number(&number.to_string()),
        Value::String(text) => escaped_string(text),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(python_json).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(items) => format!(
            "{{{}}}",
            items
                .iter()
                .map(|(key, value)| format!("{}: {}", escaped_string(key), python_json(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn dispatch(event: &str, stdin_payload: &str) -> Result<(String, String, i32), serde_json::Error> {
    let hooks = env::var("FORGE_NATIVE_HOOKS").ok();
    let answers = env::var("FORGE_NATIVE_ANSWERS").ok();
    match event {
        "PreToolUse" => Ok((
            python_json(&json!({
                "echoed":serde_json::from_str::<Value>(stdin_payload)?,
                "forge_native_hooks":hooks,
                "forge_native_answers":answers
            })),
            String::new(),
            0,
        )),
        "SessionStart" | "SessionEnd" => Ok((
            python_json(&json!({
                "event":event,
                "claude_project_dir":env::var("CLAUDE_PROJECT_DIR").ok(),
                "echoed":serde_json::from_str::<Value>(if stdin_payload.is_empty() {"{}"} else {stdin_payload})?,
                "forge_native_hooks":hooks,
                "forge_native_answers":answers
            })),
            String::new(),
            0,
        )),
        "PostToolUse" => Ok((
            python_json(
                &json!({"ok":true,"forge_native_hooks":hooks,"forge_native_answers":answers}),
            ),
            "stub dispatcher: soft warning on stderr\n".to_owned(),
            0,
        )),
        "Deny" => Ok((
            python_json(&json!({"decision":"block","reason":"stub deny"})),
            String::new(),
            2,
        )),
        "Explode" => Ok((String::new(), "stub dispatcher: fatal\n".to_owned(), 7)),
        _ => Ok((
            String::new(),
            format!(
                "stub_dispatch.py: unhandled test event {}\n",
                python_repr(event)
            ),
            1,
        )),
    }
}

fn main() {
    let event = env::args().next_back().unwrap_or_default();
    let mut stdin_payload = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut stdin_payload) {
        eprintln!("stub_dispatch.py: cannot read stdin: {error}");
        std::process::exit(1);
    }
    let (stdout, stderr, code) = match dispatch(&event, &stdin_payload) {
        Ok(output) => output,
        Err(error) => {
            eprintln!("stub_dispatch.py: invalid stdin JSON: {error}");
            std::process::exit(1);
        }
    };
    io::stdout()
        .write_all(stdout.as_bytes())
        .expect("write dispatcher stdout");
    io::stderr()
        .write_all(stderr.as_bytes())
        .expect("write dispatcher stderr");
    std::process::exit(code);
}
