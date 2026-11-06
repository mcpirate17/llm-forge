use super::{
    compile, json::Parser, shifted, unfence, CompiledSchema, Kind, Result, MAX_INPUT_BYTES,
};
use anyhow::{ensure, Context};
use serde_json::Value;

/// Immutable after registration: safe to share across worker threads without locks.
#[derive(Debug)]
pub struct ToolRegistry {
    tools: Vec<Tool>,
}

#[derive(Debug)]
struct Tool {
    name: String,
    schema: CompiledSchema,
}

/// Only the registry constructs a validated call; handlers can accept this type
/// instead of untrusted model text. Accessors borrow the registered name/input.
#[derive(Debug)]
pub struct ValidatedCall<'registry, 'input> {
    name: &'registry str,
    arguments: &'input str,
    repaired: bool,
}

impl ValidatedCall<'_, '_> {
    pub fn name(&self) -> &str {
        self.name
    }
    pub fn arguments(&self) -> &str {
        self.arguments
    }
    pub fn repaired(&self) -> bool {
        self.repaired
    }
}

impl ToolRegistry {
    /// Constrain the complete call envelope, including registered tool names.
    pub fn grammar(&self) -> String {
        super::grammar::calls(
            self.tools
                .iter()
                .map(|tool| (tool.name.as_str(), &tool.schema.root)),
        )
    }

    pub fn from_json(catalog: &str) -> anyhow::Result<Self> {
        let document = compile::document(catalog)?;
        let catalog = document.as_object().context("catalog must be an object")?;
        ensure!(
            catalog.len() == 1 && catalog.contains_key("tools"),
            "catalog requires only tools; paginated or partial catalogs must be completed first"
        );
        let entries = catalog["tools"]
            .as_array()
            .context("tools must be an array")?;
        ensure!(
            !entries.is_empty() && entries.len() <= 256,
            "catalog needs 1..256 tools"
        );
        let mut tools = Vec::with_capacity(entries.len());
        for entry in entries {
            let map = entry.as_object().context("tool must be an object")?;
            ensure!(
                map.keys()
                    .all(|key| ["name", "description", "inputSchema", "annotations"]
                        .contains(&key.as_str())),
                "unsupported tool catalog field"
            );
            if let Some(annotations) = map.get("annotations") {
                validate_annotations(annotations)?;
            }
            let name = map
                .get("name")
                .and_then(Value::as_str)
                .context("tool requires name")?;
            ensure!(
                !name.is_empty() && name.len() <= 256,
                "tool name must be 1..256 bytes"
            );
            let schema = CompiledSchema::compile(
                map.get("inputSchema")
                    .context("tool requires inputSchema")?,
            )
            .with_context(|| format!("tool {name:?}"))?;
            ensure!(
                matches!(schema.root.kind, Kind::Object { .. }),
                "tool inputSchema must be an object"
            );
            tools.push(Tool {
                name: name.to_owned(),
                schema,
            });
        }
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        ensure!(
            !tools.windows(2).any(|pair| pair[0].name == pair[1].name),
            "duplicate tool name"
        );
        Ok(Self { tools })
    }

    pub fn validate_call<'r, 'i>(
        &'r self,
        input: &'i str,
        repair: bool,
    ) -> Result<ValidatedCall<'r, 'i>> {
        match self.check(input) {
            Ok(call) => Ok(call),
            Err(original) if repair && input.len() <= MAX_INPUT_BYTES => {
                let candidate = unfence(input).ok_or(original)?;
                let mut call = self
                    .check(candidate)
                    .map_err(|e| shifted(e, input, candidate))?;
                call.repaired = true;
                Ok(call)
            }
            Err(error) => Err(error),
        }
    }

    fn check<'r, 'i>(&'r self, input: &'i str) -> Result<ValidatedCall<'r, 'i>> {
        let mut parser = Parser::new(input)?;
        parser.expect(b'{')?;
        let mut name = None;
        let mut arguments = None;
        if !parser.take(b'}') {
            loop {
                let key = parser.string()?;
                parser.expect(b':')?;
                if key.equals("name") {
                    if name.is_some() {
                        return Err(parser.error("duplicate_property"));
                    }
                    name = Some(parser.string()?);
                } else if key.equals("arguments") {
                    if arguments.is_some() {
                        return Err(parser.error("duplicate_property"));
                    }
                    parser.ws();
                    let start = parser.pos;
                    parser.skip(0)?;
                    arguments = Some(&input[start..parser.pos]);
                } else {
                    return Err(parser.error("unknown_property"));
                }
                if parser.take(b'}') {
                    break;
                }
                parser.expect(b',')?;
            }
        }
        parser.finish()?;
        let name = name.ok_or_else(|| parser.error("required_property"))?;
        let arguments = arguments.ok_or_else(|| parser.error("required_property"))?;
        let index = self
            .tools
            .binary_search_by(|tool| name.compare(&tool.name).reverse())
            .map_err(|_| parser.error("unknown_tool"))?;
        let tool = &self.tools[index];
        tool.schema
            .validate(arguments)
            .map_err(|e| shifted(e, input, arguments))?;
        Ok(ValidatedCall {
            name: &tool.name,
            arguments,
            repaired: false,
        })
    }
}

// Optional descriptive metadata never enters Tool or call validation. Only the
// bounded MCP boolean profile is accepted; absent hints acquire no defaults.
fn validate_annotations(value: &Value) -> anyhow::Result<()> {
    let annotations = value
        .as_object()
        .context("tool annotations must be an object")?;
    for (key, value) in annotations {
        ensure!(
            [
                "readOnlyHint",
                "destructiveHint",
                "openWorldHint",
                "idempotentHint"
            ]
            .contains(&key.as_str())
                && value.is_boolean(),
            "unsupported tool annotation or non-boolean hint"
        );
    }
    Ok(())
}
