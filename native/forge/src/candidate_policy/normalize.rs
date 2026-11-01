//! Ordered TOML conversion matching the Python policy adapter's date handling.
use super::{MAX_BYTES, MAX_DEPTH, MAX_NODES, MAX_STRING};
use anyhow::{ensure, Context, Result};
use serde_json::{Map, Number, Value};
use toml_edit::{DocumentMut, Item};

struct Budget {
    nodes: usize,
}
impl Budget {
    fn node(&mut self, depth: usize) -> Result<()> {
        self.nodes += 1;
        ensure!(
            depth <= MAX_DEPTH && self.nodes <= MAX_NODES,
            "candidate policy exceeds depth/node limits"
        );
        Ok(())
    }
    fn string(&self, text: &str) -> Result<()> {
        ensure!(
            text.len() <= MAX_STRING,
            "candidate policy string exceeds 4096 bytes"
        );
        Ok(())
    }
    fn table<'a>(
        &mut self,
        items: impl Iterator<Item = (&'a str, &'a Item)>,
        depth: usize,
    ) -> Result<Value> {
        self.node(depth)?;
        let mut values = Map::new();
        for (key, item) in items {
            self.string(key)?;
            values.insert(key.into(), self.item(item, depth + 1)?);
        }
        Ok(Value::Object(values))
    }
    fn item(&mut self, item: &Item, depth: usize) -> Result<Value> {
        match item {
            Item::Value(value) => self.value(value, depth),
            Item::Table(table) => self.table(table.iter(), depth),
            Item::ArrayOfTables(tables) => {
                self.node(depth)?;
                let mut result = Vec::new();
                for table in tables.iter() {
                    result.push(self.table(table.iter(), depth + 1)?);
                }
                Ok(Value::Array(result))
            }
            Item::None => anyhow::bail!("missing candidate policy TOML value"),
        }
    }
    fn value(&mut self, value: &toml_edit::Value, depth: usize) -> Result<Value> {
        self.node(depth)?;
        Ok(match value {
            toml_edit::Value::String(text) => {
                self.string(text.value())?;
                Value::String(text.value().clone())
            }
            toml_edit::Value::Integer(number) => Value::Number((*number.value()).into()),
            toml_edit::Value::Float(number) => Value::Number(
                Number::from_f64(*number.value())
                    .context("candidate policy floats must be finite")?,
            ),
            toml_edit::Value::Boolean(value) => Value::Bool(*value.value()),
            toml_edit::Value::Datetime(value) => {
                let date = value
                    .value()
                    .date
                    .context("candidate policy local times are unsupported")?;
                let date = date.to_string();
                super::validate_today(&date)?;
                Value::String(date)
            }
            toml_edit::Value::Array(array) => {
                let mut result = Vec::new();
                for value in array.iter() {
                    result.push(self.value(value, depth + 1)?);
                }
                Value::Array(result)
            }
            toml_edit::Value::InlineTable(table) => {
                let mut result = Map::new();
                for (key, value) in table.iter() {
                    self.string(key)?;
                    result.insert(key.into(), self.value(value, depth + 1)?);
                }
                Value::Object(result)
            }
        })
    }
}

pub(super) fn parse(raw: &[u8]) -> Result<Value> {
    ensure!(raw.len() <= MAX_BYTES, "candidate policy exceeds 1 MiB");
    let text = std::str::from_utf8(raw).context("candidate policy must be UTF-8")?;
    let document: DocumentMut = text
        .parse()
        .map_err(|_| anyhow::anyhow!("malformed candidate policy TOML"))?;
    Budget { nodes: 0 }.table(document.iter(), 0)
}

pub(super) fn validate_json(value: &Value) -> Result<()> {
    fn visit(value: &Value, depth: usize, budget: &mut Budget) -> Result<()> {
        budget.node(depth)?;
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    budget.string(key)?;
                    visit(value, depth + 1, budget)?;
                }
            }
            Value::Array(items) => {
                for value in items {
                    visit(value, depth + 1, budget)?;
                }
            }
            Value::String(text) => budget.string(text)?,
            _ => {}
        }
        Ok(())
    }
    visit(value, 0, &mut Budget { nodes: 0 })
}
