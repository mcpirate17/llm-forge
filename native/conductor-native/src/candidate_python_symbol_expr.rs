use super::*;
macro_rules! form {
    ($this:expr, $kind:expr, $depth:expr, $( $name:expr => $value:expr ),* $(,)?) => {{
        let fields = vec![$(($name, $value)),*];
        $this.form($kind, fields, $depth)
    }};
}
impl Dump<'_> {
    pub(super) fn expr(&mut self, value: &ast::Expr, d: usize) -> Result<String, String> {
        Self::check_depth(d)?;
        use ast::Expr as E;
        match value {
            E::Name(v) => {
                form!(self,"Name",d, "id" => self.ident(v.id.as_str(),d+1)?, "ctx" => context(v.ctx)?)
            }
            E::Attribute(v) => {
                form!(self,"Attribute",d, "value" => self.expr(&v.value,d+1)?, "attr" => self.ident(&v.attr,d+1)?, "ctx" => context(v.ctx)?)
            }
            E::Subscript(v) => {
                form!(self,"Subscript",d, "value" => self.expr(&v.value,d+1)?, "slice" => self.expr(&v.slice,d+1)?, "ctx" => context(v.ctx)?)
            }
            E::Starred(v) => {
                form!(self,"Starred",d, "value" => self.expr(&v.value,d+1)?, "ctx" => context(v.ctx)?)
            }
            E::List(v) => {
                form!(self,"List",d, "elts" => self.exprs(&v.elts,d+1)?, "ctx" => context(v.ctx)?)
            }
            E::Tuple(v) => {
                form!(self,"Tuple",d, "elts" => self.exprs(&v.elts,d+1)?, "ctx" => context(v.ctx)?)
            }
            E::Set(v) => form!(self,"Set",d, "elts" => self.exprs(&v.elts,d+1)?),
            E::BinOp(v) => {
                form!(self,"BinOp",d, "left" => self.expr(&v.left,d+1)?, "op" => format!("{:?}()",v.op), "right" => self.expr(&v.right,d+1)?)
            }
            E::UnaryOp(v) => {
                form!(self,"UnaryOp",d, "op" => format!("{:?}()",v.op), "operand" => self.expr(&v.operand,d+1)?)
            }
            E::BoolOp(v) => {
                form!(self,"BoolOp",d, "op" => format!("{:?}()",v.op), "values" => self.exprs(&v.values,d+1)?)
            }
            E::Named(v) => {
                form!(self,"NamedExpr",d, "target" => self.expr(&v.target,d+1)?, "value" => self.expr(&v.value,d+1)?)
            }
            E::If(v) => {
                form!(self,"IfExp",d, "test" => self.expr(&v.test,d+1)?, "body" => self.expr(&v.body,d+1)?, "orelse" => self.expr(&v.orelse,d+1)?)
            }
            E::Await(v) => form!(self,"Await",d, "value" => self.expr(&v.value,d+1)?),
            E::YieldFrom(v) => form!(self,"YieldFrom",d, "value" => self.expr(&v.value,d+1)?),
            E::Yield(v) => {
                let mut f = vec![];
                self.optional_expr(&mut f, "value", v.value.as_deref(), d)?;
                self.form("Yield", f, d)
            }
            E::Slice(v) => self.slice(v, d),
            E::Call(v) => self.call(v, d),
            E::Compare(v) => self.compare(v, d),
            E::Dict(v) => self.dict(v, d),
            E::Lambda(v) => {
                let args = self.parameters(v.parameters.as_deref(), d + 1)?;
                form!(self,"Lambda",d,"args" => args,"body" => self.expr(&v.body,d+1)?)
            }
            E::ListComp(v) => self.comprehension("ListComp", &v.elt, &v.generators, d),
            E::SetComp(v) => self.comprehension("SetComp", &v.elt, &v.generators, d),
            E::Generator(v) => self.comprehension("GeneratorExp", &v.elt, &v.generators, d),
            E::DictComp(v) => {
                let gs = self.generators(&v.generators, d + 1)?;
                form!(self,"DictComp",d,"key"=>self.expr(&v.key,d+1)?,"value"=>self.expr(&v.value,d+1)?,"generators"=>gs)
            }
            E::StringLiteral(v) => {
                let mut f = vec![("value", self.repr(v.value.to_str(), d + 1)?)];
                if v.value.is_unicode() {
                    f.push(("kind", "'u'".into()));
                }
                self.form("Constant", f, d)
            }
            E::NumberLiteral(v) => {
                let number = number(&v.value, &mut self.work)?;
                self.form("Constant", vec![("value", number)], d)
            }
            E::BooleanLiteral(v) => self.form(
                "Constant",
                vec![("value", if v.value { "True" } else { "False" }.into())],
                d,
            ),
            E::NoneLiteral(_) => self.form("Constant", vec![("value", "None".into())], d),
            E::EllipsisLiteral(_) => self.form("Constant", vec![("value", "Ellipsis".into())], d),
            E::BytesLiteral(v) => {
                let value = bytes_repr(v.value.bytes().collect());
                self.form("Constant", vec![("value", value)], d)
            }
            E::FString(v) => self.fstring(v, d),
            E::TString(_) | E::IpyEscapeCommand(_) => {
                Err("unsupported CPython 3.12 literal or extension in symbol".into())
            }
        }
    }
    fn fstring(&mut self, value: &ast::ExprFString, d: usize) -> Result<String, String> {
        let mut out = Vec::new();
        let mut pending: Option<(String, bool)> = None;
        for part in &value.value {
            match part {
                ast::FStringPart::Literal(literal) => append_literal(
                    &mut pending,
                    &literal.value,
                    literal.flags.prefix().is_unicode(),
                ),
                ast::FStringPart::FString(fstring) => {
                    self.f_elements(&fstring.elements, &mut out, &mut pending, d + 1)?
                }
            }
        }
        self.flush_literal(&mut out, &mut pending, d + 1)?;
        let values = self.list(out, d + 1)?;
        self.form("JoinedStr", vec![("values", values)], d)
    }
    fn f_elements(
        &mut self,
        elements: &ast::InterpolatedStringElements,
        out: &mut Vec<String>,
        pending: &mut Option<(String, bool)>,
        d: usize,
    ) -> Result<(), String> {
        Self::check_depth(d)?;
        for element in elements {
            match element {
                ast::InterpolatedStringElement::Literal(literal) => {
                    append_literal(pending, &literal.value, false)
                }
                ast::InterpolatedStringElement::Interpolation(value) => {
                    if let Some(debug) = &value.debug_text {
                        use ruff_text_size::Ranged;
                        let range = value.expression.range();
                        let expression = self
                            .source
                            .get(range.start().to_usize()..range.end().to_usize())
                            .ok_or("f-string source range invalid")?;
                        append_literal(
                            pending,
                            &format!("{}{}{}", debug.leading, expression, debug.trailing),
                            false,
                        );
                    }
                    self.flush_literal(out, pending, d + 1)?;
                    let expression = self.expr(&value.expression, d + 1)?;
                    let conversion = if value.debug_text.is_some()
                        && value.format_spec.is_none()
                        && matches!(value.conversion, ast::ConversionFlag::None)
                    {
                        114
                    } else {
                        value.conversion as i8
                    };
                    let mut fields = vec![
                        ("value", expression),
                        ("conversion", conversion.to_string()),
                    ];
                    if let Some(spec) = &value.format_spec {
                        let mut children = Vec::new();
                        let mut text = None;
                        self.f_elements(&spec.elements, &mut children, &mut text, d + 2)?;
                        self.flush_literal(&mut children, &mut text, d + 2)?;
                        // CPython 3.12 retains the empty format-spec tail after a
                        // replacement field, unlike an outer JoinedStr or an empty spec.
                        if matches!(
                            spec.elements.iter().last(),
                            Some(ast::InterpolatedStringElement::Interpolation(_))
                        ) {
                            children.push(self.form(
                                "Constant",
                                vec![("value", "''".into())],
                                d + 2,
                            )?);
                        }
                        let children = self.list(children, d + 2)?;
                        fields.push((
                            "format_spec",
                            self.form("JoinedStr", vec![("values", children)], d + 1)?,
                        ));
                    }
                    out.push(self.form("FormattedValue", fields, d + 1)?);
                }
            }
        }
        Ok(())
    }
    fn flush_literal(
        &mut self,
        out: &mut Vec<String>,
        pending: &mut Option<(String, bool)>,
        d: usize,
    ) -> Result<(), String> {
        if let Some((text, unicode)) = pending.take() {
            if text.is_empty() {
                return Ok(());
            }
            let mut fields = vec![("value", self.repr(&text, d + 1)?)];
            if unicode {
                fields.push(("kind", "'u'".into()));
            }
            out.push(self.form("Constant", fields, d)?);
        }
        Ok(())
    }
    fn slice(&mut self, v: &ast::ExprSlice, d: usize) -> Result<String, String> {
        let mut f = vec![];
        self.optional_expr(&mut f, "lower", v.lower.as_deref(), d)?;
        self.optional_expr(&mut f, "upper", v.upper.as_deref(), d)?;
        self.optional_expr(&mut f, "step", v.step.as_deref(), d)?;
        self.form("Slice", f, d)
    }
    fn call(&mut self, v: &ast::ExprCall, d: usize) -> Result<String, String> {
        let func = self.expr(&v.func, d + 1)?;
        let args = self.exprs(&v.arguments.args, d + 1)?;
        let keywords = self.keywords(&v.arguments.keywords, d + 1)?;
        self.form(
            "Call",
            vec![("func", func), ("args", args), ("keywords", keywords)],
            d,
        )
    }
    pub(super) fn keywords(&mut self, values: &[ast::Keyword], d: usize) -> Result<String, String> {
        let mut out = Vec::new();
        for value in values {
            let mut f = vec![];
            if let Some(name) = &value.arg {
                f.push(("arg", self.ident(name, d + 1)?));
            }
            f.push(("value", self.expr(&value.value, d + 1)?));
            out.push(self.form("keyword", f, d + 1)?);
        }
        self.list(out, d)
    }
    fn compare(&mut self, v: &ast::ExprCompare, d: usize) -> Result<String, String> {
        let left = self.expr(&v.left, d + 1)?;
        let ops = self.list(
            v.ops
                .iter()
                .map(|op| format!("{op:?}() ").trim_end().to_owned())
                .collect(),
            d + 1,
        )?;
        let comparators = self.exprs(&v.comparators, d + 1)?;
        self.form(
            "Compare",
            vec![("left", left), ("ops", ops), ("comparators", comparators)],
            d,
        )
    }
    fn dict(&mut self, v: &ast::ExprDict, d: usize) -> Result<String, String> {
        let mut keys = Vec::new();
        let mut values = Vec::new();
        for item in &v.items {
            keys.push(match &item.key {
                Some(key) => self.expr(key, d + 1)?,
                None => "None".into(),
            });
            values.push(self.expr(&item.value, d + 1)?);
        }
        let keys = self.list(keys, d + 1)?;
        let values = self.list(values, d + 1)?;
        self.form("Dict", vec![("keys", keys), ("values", values)], d)
    }
    fn generators(&mut self, values: &[ast::Comprehension], d: usize) -> Result<String, String> {
        let mut out = Vec::new();
        for v in values {
            let target = self.expr(&v.target, d + 1)?;
            let iter = self.expr(&v.iter, d + 1)?;
            let ifs = self.exprs(&v.ifs, d + 1)?;
            let is_async = if v.is_async { "1" } else { "0" }.into();
            out.push(self.form(
                "comprehension",
                vec![
                    ("target", target),
                    ("iter", iter),
                    ("ifs", ifs),
                    ("is_async", is_async),
                ],
                d + 1,
            )?);
        }
        self.list(out, d)
    }
    fn comprehension(
        &mut self,
        kind: &str,
        elt: &ast::Expr,
        generators: &[ast::Comprehension],
        d: usize,
    ) -> Result<String, String> {
        let elt = self.expr(elt, d + 1)?;
        let gs = self.generators(generators, d + 1)?;
        self.form(kind, vec![("elt", elt), ("generators", gs)], d)
    }
}
fn context(value: ast::ExprContext) -> Result<String, String> {
    if matches!(value, ast::ExprContext::Invalid) {
        return Err("invalid expression context".into());
    }
    Ok(format!("{value:?}()"))
}
fn number(value: &ast::Number, work: &mut usize) -> Result<String, String> {
    match value {
        ast::Number::Int(value) => integer_repr(value, work),
        ast::Number::Float(value) => float_repr(*value),
        ast::Number::Complex { real, imag } => {
            if *real != 0.0 {
                return Err("unsupported nonliteral complex value".into());
            }
            let mut value = float_repr(*imag)?;
            if value.ends_with(".0") {
                value.truncate(value.len() - 2);
            }
            value.push('j');
            Ok(value)
        }
    }
}
fn float_repr(value: f64) -> Result<String, String> {
    if value.is_nan() {
        return Err("NaN literal unsupported".into());
    }
    if value.is_infinite() {
        return Ok(if value.is_sign_negative() {
            "-inf"
        } else {
            "inf"
        }
        .into());
    }
    if value == 0.0 {
        return Ok(if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into());
    }
    let shortest = serde_json::Number::from_f64(value.abs())
        .ok_or("invalid float")?
        .to_string();
    let (mantissa, exp) = shortest.split_once(['e', 'E']).unwrap_or((&shortest, "0"));
    let exp: i32 = exp.parse().map_err(|_| "invalid float exponent")?;
    let decimal = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let leading = digits.bytes().take_while(|b| *b == b'0').count();
    if leading == digits.len() {
        return Err("invalid float digits".into());
    }
    let exponent = decimal + exp - leading as i32 - 1;
    let significant = digits[leading..].trim_end_matches('0');
    let mut result = if (-4..16).contains(&exponent) {
        fixed(significant, exponent)
    } else {
        let mut x = significant[..1].to_owned();
        if significant.len() > 1 {
            x.push('.');
            x.push_str(&significant[1..]);
        }
        x.push('e');
        x.push(if exponent < 0 { '-' } else { '+' });
        x.push_str(&format!("{:02}", exponent.unsigned_abs()));
        x
    };
    if value.is_sign_negative() {
        result.insert(0, '-');
    }
    Ok(result)
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

fn bytes_repr(bytes: Vec<u8>) -> String {
    use std::fmt::Write;
    let quote = if bytes.contains(&b'\'') && !bytes.contains(&b'"') {
        b'"'
    } else {
        b'\''
    };
    let mut out = String::from("b");
    out.push(quote as char);
    for byte in bytes {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            x if x == quote => {
                out.push('\\');
                out.push(x as char);
            }
            0x20..=0x7e => out.push(byte as char),
            x => {
                write!(&mut out, "\\x{x:02x}").expect("writing to String");
            }
        }
    }
    out.push(quote as char);
    out
}
fn integer_repr(value: &ast::Int, work: &mut usize) -> Result<String, String> {
    if let Some(value) = value.as_u64() {
        return Ok(value.to_string());
    }
    let raw = value.to_string();
    let compact = raw.replace('_', "");
    let (base, digits) = if let Some(v) = compact
        .strip_prefix("0x")
        .or_else(|| compact.strip_prefix("0X"))
    {
        (16u64, v)
    } else if let Some(v) = compact
        .strip_prefix("0o")
        .or_else(|| compact.strip_prefix("0O"))
    {
        (8, v)
    } else if let Some(v) = compact
        .strip_prefix("0b")
        .or_else(|| compact.strip_prefix("0B"))
    {
        (2, v)
    } else {
        (10, compact.as_str())
    };
    if digits.len() > 14_284 {
        return Err("integer exceeds CPython 3.12 conversion limit".into());
    }
    let mut parts = vec![0u64];
    for ch in digits.chars() {
        let mut carry = ch.to_digit(base as u32).ok_or("invalid integer")? as u64;
        *work = work
            .checked_add(parts.len())
            .ok_or("integer work overflow")?;
        if *work > MAX_WORK {
            return Err("integer conversion exceeds work budget".into());
        }
        for part in &mut parts {
            let n = *part * base + carry;
            *part = n % 1_000_000_000;
            carry = n / 1_000_000_000;
        }
        if carry > 0 {
            parts.push(carry);
        }
        if parts.len() > 478 {
            return Err("integer exceeds CPython 3.12 decimal conversion limit".into());
        }
    }
    let mut iter = parts.iter().rev();
    let mut result = iter.next().ok_or("empty integer")?.to_string();
    use std::fmt::Write;
    for part in iter {
        write!(&mut result, "{part:09}").map_err(|e| e.to_string())?;
    }
    if result.len() > 4300 {
        return Err("integer exceeds CPython 3.12 decimal conversion limit".into());
    }
    Ok(result)
}

fn append_literal(pending: &mut Option<(String, bool)>, text: &str, unicode: bool) {
    match pending {
        Some((value, _)) => value.push_str(text),
        None => *pending = Some((text.to_owned(), unicode)),
    }
}
