use super::{json::Parser, number::Decimal, Kind, Node, Property, MAX_DEPTH};
use anyhow::{bail, ensure, Context};
use serde_json::{Map, Value};
use std::collections::HashSet;

/// Registration-only parse: reject duplicates before Value's last-key-wins
/// behavior can change a schema, catalog or tool name.
pub(super) fn document(text: &str) -> anyhow::Result<Value> {
    let mut parser = Parser::new(text)?;
    unique_keys(&mut parser, 0)?;
    parser.finish()?;
    Ok(serde_json::from_str(text)?)
}

fn unique_keys(parser: &mut Parser<'_>, depth: usize) -> anyhow::Result<()> {
    ensure!(depth <= MAX_DEPTH * 2 + 8, "schema document depth limit");
    match parser.peek() {
        Some(b'{') => {
            parser.expect(b'{')?;
            let mut keys = HashSet::new();
            if parser.take(b'}') {
                return Ok(());
            }
            loop {
                let key: String = parser.string()?.chars().collect();
                ensure!(
                    keys.insert(key.clone()),
                    "duplicate schema/catalog key {key:?}"
                );
                parser.expect(b':')?;
                unique_keys(parser, depth + 1)?;
                if parser.take(b'}') {
                    break;
                }
                parser.expect(b',')?;
            }
        }
        Some(b'[') => {
            parser.expect(b'[')?;
            if parser.take(b']') {
                return Ok(());
            }
            loop {
                unique_keys(parser, depth + 1)?;
                if parser.take(b']') {
                    break;
                }
                parser.expect(b',')?;
            }
        }
        _ => parser.skip(0)?,
    }
    Ok(())
}

pub(super) fn compile(schema: &Value) -> anyhow::Result<Node> {
    build(schema, 0, &mut 0).context("tool schema")
}

fn build(schema: &Value, depth: usize, nodes: &mut usize) -> anyhow::Result<Node> {
    *nodes += 1;
    ensure!(
        depth <= MAX_DEPTH && *nodes <= 4096,
        "schema depth/node limit exceeded"
    );
    let map = schema
        .as_object()
        .context("expected schema object with explicit type")?;
    let kind = map
        .get("type")
        .and_then(Value::as_str)
        .context("type must be a single string")?;
    keywords(map, kind)?;
    let kind = match kind {
        "object" => object(map, depth, nodes)?,
        "array" => {
            let (min, max) = lengths(map, "minItems", "maxItems")?;
            let items = build(
                map.get("items").context("array requires items")?,
                depth + 1,
                nodes,
            )
            .context("items")?;
            Kind::Array {
                items: Box::new(items),
                min,
                max,
            }
        }
        "string" => string(map)?,
        "number" | "integer" => number(map, kind == "integer")?,
        "boolean" => Kind::Boolean,
        "null" => Kind::Null,
        _ => bail!("unsupported type {kind:?}"),
    };
    Ok(Node { kind })
}

fn keywords(map: &Map<String, Value>, kind: &str) -> anyhow::Result<()> {
    let specific: &[&str] = match kind {
        "object" => &["properties", "required", "additionalProperties"],
        "array" => &["items", "minItems", "maxItems"],
        "string" => &["minLength", "maxLength", "enum", "const"],
        "number" | "integer" => &["minimum", "maximum"],
        _ => &[],
    };
    for key in map.keys() {
        ensure!(
            [
                "type",
                "$schema",
                "title",
                "description",
                "default",
                "examples",
                "deprecated"
            ]
            .contains(&key.as_str())
                || specific.contains(&key.as_str()),
            "unsupported keyword {key:?} for {kind}"
        );
    }
    if let Some(dialect) = map.get("$schema") {
        ensure!(
            dialect.as_str() == Some("https://json-schema.org/draft/2020-12/schema"),
            "only JSON Schema 2020-12 is supported"
        );
    }
    Ok(())
}

fn object(map: &Map<String, Value>, depth: usize, nodes: &mut usize) -> anyhow::Result<Kind> {
    ensure!(
        map.get("additionalProperties") == Some(&Value::Bool(false)),
        "tool objects require explicit additionalProperties: false"
    );
    let empty = Map::new();
    let properties = match map.get("properties") {
        Some(value) => value.as_object().context("properties must be an object")?,
        None => &empty,
    };
    ensure!(properties.len() <= 128, "at most 128 properties per object");
    let mut properties: Vec<_> = properties
        .iter()
        .map(|(name, value)| {
            ensure!(name.len() <= 256, "property name exceeds 256 bytes");
            Ok(Property {
                name: name.clone(),
                node: build(value, depth + 1, nodes)
                    .with_context(|| format!("property {name:?}"))?,
            })
        })
        .collect::<anyhow::Result<_>>()?;
    properties.sort_by(|a, b| a.name.cmp(&b.name));
    let mut required = 0u128;
    if let Some(value) = map.get("required") {
        for item in value.as_array().context("required must be an array")? {
            let name = item.as_str().context("required entries must be strings")?;
            let index = properties
                .iter()
                .position(|p| p.name == name)
                .with_context(|| format!("required property {name:?} is undeclared"))?;
            let bit = 1u128 << index;
            ensure!(required & bit == 0, "duplicate required property {name:?}");
            required |= bit;
        }
    }
    Ok(Kind::Object {
        properties,
        required,
    })
}

fn lengths(map: &Map<String, Value>, lo: &str, hi: &str) -> anyhow::Result<(usize, usize)> {
    let read = |key, default| -> anyhow::Result<usize> {
        let Some(value) = map.get(key) else {
            return Ok(default);
        };
        let value = value
            .as_u64()
            .with_context(|| format!("{key} must be a nonnegative integer"))?;
        let limit = if hi == "maxLength" {
            super::MAX_INPUT_BYTES
        } else {
            4096
        };
        ensure!(
            value <= limit as u64,
            "{key} exceeds bounded grammar limit {limit}"
        );
        Ok(value as usize)
    };
    let min = read(lo, 0)?;
    let max = read(hi, usize::MAX)?;
    ensure!(min <= max, "{lo} exceeds {hi}");
    Ok((min, max))
}

fn string(map: &Map<String, Value>) -> anyhow::Result<Kind> {
    let (min, max) = lengths(map, "minLength", "maxLength")?;
    ensure!(
        !(map.contains_key("enum") && map.contains_key("const")),
        "use either enum or const"
    );
    let mut choices = if let Some(value) = map.get("enum") {
        Some(
            value
                .as_array()
                .context("enum must be an array")?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .context("only string enum values are supported")
                })
                .collect::<anyhow::Result<Vec<_>>>()?,
        )
    } else if let Some(value) = map.get("const") {
        Some(vec![value
            .as_str()
            .context("only string const is supported")?
            .to_owned()])
    } else {
        None
    };
    if let Some(values) = choices.as_mut() {
        ensure!(
            !values.is_empty() && values.len() <= 256,
            "enum needs 1..256 choices"
        );
        for value in values.iter() {
            ensure!(
                value.len() <= 4096 && (min..=max).contains(&value.chars().count()),
                "enum choice exceeds length constraints"
            );
        }
        values.sort();
        ensure!(
            !values.windows(2).any(|pair| pair[0] == pair[1]),
            "duplicate enum choice"
        );
    }
    Ok(Kind::String { min, max, choices })
}

fn number(map: &Map<String, Value>, integer: bool) -> anyhow::Result<Kind> {
    let bound = |key| -> anyhow::Result<Option<String>> {
        let Some(value) = map.get(key) else {
            return Ok(None);
        };
        ensure!(value.is_number(), "{key} must be numeric");
        let text = value.to_string();
        let mut parser = Parser::new(&text)?;
        parser.number()?;
        parser.finish()?;
        Ok(Some(text))
    };
    let min = bound("minimum")?;
    let max = bound("maximum")?;
    if let (Some(a), Some(b)) = (&min, &max) {
        ensure!(
            !Decimal::new(a).compare(&Decimal::new(b)).is_gt(),
            "minimum exceeds maximum"
        );
    }
    Ok(Kind::Number { integer, min, max })
}
