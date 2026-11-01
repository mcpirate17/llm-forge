//! CPython json.dumps canonical bytes for finite JSON numbers.
use anyhow::{ensure, Context, Result};
use serde_json::{Number, Value};

const INTEGER_DIGITS: usize = 4300;
const FLOAT_TOKEN_BYTES: usize = 16_384;

pub(super) fn encode(value: &Value) -> Result<String> {
    let mut output = String::new();
    write(value, &mut output)?;
    Ok(output)
}
fn write(value: &Value, out: &mut String) -> Result<()> {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key)?);
                out.push(':');
                write(&map[key], out)?;
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index != 0 {
                    out.push(',');
                }
                write(item, out)?;
            }
            out.push(']');
        }
        Value::Number(number) => out.push_str(&number_repr(number)?),
        _ => out.push_str(&serde_json::to_string(value)?),
    }
    Ok(())
}
fn number_repr(number: &Number) -> Result<String> {
    let token = number.to_string();
    if !token.contains(['.', 'e', 'E']) {
        ensure!(
            token.trim_start_matches('-').len() <= INTEGER_DIGITS,
            "candidate integer exceeds 4300 digits"
        );
        return Ok(if token == "-0" { "0".into() } else { token });
    }
    ensure!(
        token.len() <= FLOAT_TOKEN_BYTES,
        "candidate floating-point token exceeds limit"
    );
    let value: f64 = token
        .parse()
        .context("invalid candidate floating-point number")?;
    ensure!(
        value.is_finite(),
        "candidate floating-point number must be finite"
    );
    if value == 0.0 {
        return Ok(if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into());
    }
    let shortest = Number::from_f64(value.abs())
        .context("invalid finite float")?
        .to_string();
    let (digits, exponent) = digits_and_exponent(&shortest)?;
    let mut result = if (-4..16).contains(&exponent) {
        fixed(&digits, exponent)
    } else {
        let mut result = digits[..1].to_owned();
        if digits.len() > 1 {
            result.push('.');
            result.push_str(&digits[1..]);
        }
        result.push('e');
        result.push(if exponent < 0 { '-' } else { '+' });
        result.push_str(&format!("{:02}", exponent.unsigned_abs()));
        result
    };
    if value.is_sign_negative() {
        result.insert(0, '-');
    }
    Ok(result)
}
fn digits_and_exponent(shortest: &str) -> Result<(String, i32)> {
    let (mantissa, exponent) = shortest.split_once(['e', 'E']).unwrap_or((shortest, "0"));
    let exponent: i32 = exponent
        .parse()
        .context("invalid shortest float exponent")?;
    let decimal = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let leading = digits.bytes().take_while(|b| *b == b'0').count();
    ensure!(leading < digits.len(), "unexpected zero shortest float");
    let exp = decimal + exponent - leading as i32 - 1;
    let significant = digits[leading..].trim_end_matches('0');
    Ok((significant.to_owned(), exp))
}
fn fixed(digits: &str, exponent: i32) -> String {
    let point = exponent + 1;
    if point <= 0 {
        return format!("0.{}{}", "0".repeat((-point) as usize), digits);
    }
    let point = point as usize;
    if point >= digits.len() {
        return format!("{}{}.0", digits, "0".repeat(point - digits.len()));
    }
    format!("{}.{}", &digits[..point], &digits[point..])
}
