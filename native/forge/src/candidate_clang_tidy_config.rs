//! Bounded YAML-to-JSON clang-tidy config admission. Candidate compiler-extra
//! arguments and parent config inheritance are explicitly unsupported.
use anyhow::{bail, ensure, Context, Result};
use serde_json::{Map, Number, Value};
use yaml_rust2::{
    parser::{Event, Parser},
    scanner::TScalarStyle,
    Yaml,
};

const MAX_BYTES: usize = 64 * 1024;
const MAX_EVENTS: usize = 20_000;
const MAX_DEPTH: usize = 32;

enum Frame {
    Sequence(Vec<Value>),
    Mapping {
        values: Map<String, Value>,
        key: Option<String>,
    },
}

fn scalar(value: String, style: TScalarStyle) -> Result<Value> {
    if style != TScalarStyle::Plain {
        return Ok(Value::String(value));
    }
    Ok(match Yaml::from_str(&value) {
        Yaml::String(_) => Value::String(value),
        Yaml::Boolean(b) => Value::Bool(b),
        Yaml::Integer(i) => Value::Number(i.into()),
        Yaml::Null => Value::Null,
        Yaml::Real(s) => {
            let number = s
                .parse::<f64>()
                .context("unsupported YAML numeric scalar")?;
            Value::Number(Number::from_f64(number).context("nonfinite YAML number")?)
        }
        _ => bail!("unsupported YAML scalar"),
    })
}

fn scalar_event(stack: &[Frame], value: String, style: TScalarStyle) -> Result<Value> {
    let mapping_key = matches!(stack.last(), Some(Frame::Mapping { key: None, .. }));
    let inherited = stack.len() == 1
        && matches!(stack.last(), Some(Frame::Mapping { key: Some(key), .. }) if key == "InheritParentConfig");
    if mapping_key || inherited {
        scalar(value, style)
    } else {
        Ok(Value::String(value))
    }
}

fn append(stack: &mut [Frame], root: &mut Option<Value>, value: Value) -> Result<()> {
    match stack.last_mut() {
        None => {
            ensure!(
                root.replace(value).is_none(),
                "multiple YAML document roots"
            );
        }
        Some(Frame::Sequence(values)) => values.push(value),
        Some(Frame::Mapping { values, key }) => match key.take() {
            None => {
                let Value::String(next) = value else {
                    bail!("YAML mapping keys must be strings");
                };
                ensure!(!values.contains_key(&next), "duplicate YAML mapping key");
                *key = Some(next);
            }
            Some(key) => {
                ensure!(
                    values.insert(key, value).is_none(),
                    "duplicate YAML mapping key"
                );
            }
        },
    }
    Ok(())
}

fn start(stack: &mut Vec<Frame>, frame: Frame) -> Result<()> {
    ensure!(stack.len() < MAX_DEPTH, "YAML nesting exceeds 32");
    if let Some(Frame::Mapping { key: None, .. }) = stack.last() {
        bail!("YAML mapping keys must be scalar strings");
    }
    stack.push(frame);
    Ok(())
}

fn finish(stack: &mut Vec<Frame>, root: &mut Option<Value>, sequence: bool) -> Result<()> {
    let frame = stack.pop().context("unexpected YAML container end")?;
    let value = match frame {
        Frame::Sequence(values) if sequence => Value::Array(values),
        Frame::Mapping { values, key: None } if !sequence => Value::Object(values),
        _ => bail!("invalid YAML container or incomplete mapping"),
    };
    append(stack, root, value)
}

fn decode(text: &str) -> Result<Value> {
    let mut parser = Parser::new_from_str(text);
    let mut stack = Vec::new();
    let mut root = None;
    let mut documents = 0usize;
    for _ in 0..MAX_EVENTS {
        let (event, _) = parser
            .next_token()
            .map_err(|_| anyhow::anyhow!("invalid YAML configuration"))?;
        match event {
            Event::StreamEnd => {
                ensure!(
                    stack.is_empty() && documents <= 1,
                    "invalid YAML document structure"
                );
                return Ok(root.unwrap_or_else(|| Value::Object(Map::new())));
            }
            Event::DocumentStart => {
                documents += 1;
                ensure!(documents == 1, "multiple YAML documents are unsupported");
            }
            Event::Alias(_) => bail!("YAML aliases are unsupported"),
            Event::Scalar(value, style, anchor, tag) => {
                ensure!(
                    anchor == 0 && tag.is_none(),
                    "YAML anchors and tags are unsupported"
                );
                let parsed = scalar_event(&stack, value, style)?;
                append(&mut stack, &mut root, parsed)?;
            }
            Event::SequenceStart(anchor, tag) | Event::MappingStart(anchor, tag)
                if anchor != 0 || tag.is_some() =>
            {
                bail!("YAML anchors and tags are unsupported")
            }
            Event::SequenceStart(_, _) => start(&mut stack, Frame::Sequence(Vec::new()))?,
            Event::MappingStart(_, _) => start(
                &mut stack,
                Frame::Mapping {
                    values: Map::new(),
                    key: None,
                },
            )?,
            Event::SequenceEnd => finish(&mut stack, &mut root, true)?,
            Event::MappingEnd => finish(&mut stack, &mut root, false)?,
            _ => {}
        }
    }
    bail!("YAML configuration exceeds event limit")
}

pub fn sanitize(raw: &[u8]) -> Result<Vec<u8>> {
    ensure!(raw.len() <= MAX_BYTES, "clang-tidy config exceeds 64 KiB");
    let text = std::str::from_utf8(raw).context("clang-tidy config must be UTF-8")?;
    let mut value = decode(text)?;
    let object = value
        .as_object_mut()
        .context("clang-tidy config must be a mapping")?;
    for key in ["ExtraArgs", "ExtraArgsBefore", "RemovedArgs"] {
        ensure!(
            !object.contains_key(key),
            "clang-tidy compiler argument options are unsupported"
        );
    }
    match object.get("InheritParentConfig") {
        None | Some(Value::Bool(false)) => {}
        _ => bail!("clang-tidy parent config inheritance is unsupported"),
    }
    object.insert("InheritParentConfig".into(), Value::Bool(false));
    let encoded = serde_json::to_vec(&value)?;
    ensure!(
        encoded.len() <= MAX_BYTES,
        "sanitized clang-tidy config exceeds 64 KiB"
    );
    Ok(encoded)
}

#[cfg(test)]
#[path = "candidate_clang_tidy_config_tests.rs"]
mod tests;
