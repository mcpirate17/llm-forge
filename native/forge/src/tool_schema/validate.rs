use super::{json::Parser, number::Decimal, Kind, Node, Property, Result, MAX_DEPTH};
use std::cmp::Ordering;

pub(super) fn node(schema: &Node, parser: &mut Parser<'_>, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(parser.error("depth_limit"));
    }
    match &schema.kind {
        Kind::Object {
            properties,
            required,
        } => object(properties, *required, parser, depth),
        Kind::Array { items, min, max } => array(items, *min, *max, parser, depth),
        Kind::String { min, max, choices } => {
            let value = parser.string()?;
            if !(*min..=*max).contains(&value.len) {
                return Err(parser.error("string_length"));
            }
            if choices
                .as_ref()
                .is_some_and(|values| !values.iter().any(|s| value.equals(s)))
            {
                return Err(parser.error("enum"));
            }
            Ok(())
        }
        Kind::Number { integer, min, max } => {
            let value = Decimal::new(parser.number()?);
            if *integer && !value.integer() {
                return Err(parser.error("integer"));
            }
            if min
                .as_ref()
                .is_some_and(|s| value.compare(&Decimal::new(s)) == Ordering::Less)
                || max
                    .as_ref()
                    .is_some_and(|s| value.compare(&Decimal::new(s)) == Ordering::Greater)
            {
                return Err(parser.error("number_range"));
            }
            Ok(())
        }
        Kind::Boolean => match parser.peek() {
            Some(b't') => parser.literal("true"),
            _ => parser.literal("false"),
        },
        Kind::Null => parser.literal("null"),
    }
}

fn object(
    properties: &[Property],
    required: u128,
    parser: &mut Parser<'_>,
    depth: usize,
) -> Result<()> {
    parser.expect(b'{')?;
    let mut seen = 0u128;
    if !parser.take(b'}') {
        loop {
            let key = parser.string()?;
            let index = properties
                .binary_search_by(|p| key.compare(&p.name).reverse())
                .map_err(|_| parser.error("unknown_property"))?;
            let bit = 1u128 << index;
            if seen & bit != 0 {
                return Err(parser.error("duplicate_property"));
            }
            seen |= bit;
            parser.expect(b':')?;
            node(&properties[index].node, parser, depth + 1)?;
            if parser.take(b'}') {
                break;
            }
            parser.expect(b',')?;
        }
    }
    if seen & required != required {
        return Err(parser.error("required_property"));
    }
    Ok(())
}

fn array(
    items: &Node,
    min: usize,
    max: usize,
    parser: &mut Parser<'_>,
    depth: usize,
) -> Result<()> {
    parser.expect(b'[')?;
    let mut count = 0;
    if !parser.take(b']') {
        loop {
            if count == max {
                return Err(parser.error("array_length"));
            }
            node(items, parser, depth + 1)?;
            count += 1;
            if parser.take(b']') {
                break;
            }
            parser.expect(b',')?;
        }
    }
    if count < min {
        return Err(parser.error("array_length"));
    }
    Ok(())
}
