//! Bounded JSON scanner. Escapes are decoded as iterators, never into a buffer.
use super::{Result, ValidationError, MAX_DEPTH, MAX_INPUT_BYTES};

pub(super) struct Parser<'a> {
    pub input: &'a str,
    pub pos: usize,
}

#[derive(Clone, Copy)]
pub(super) struct JsonString<'a> {
    raw: &'a str,
    pub len: usize,
}

impl JsonString<'_> {
    pub fn equals(&self, text: &str) -> bool {
        self.chars().eq(text.chars())
    }

    pub fn compare(&self, text: &str) -> std::cmp::Ordering {
        self.chars().cmp(text.chars())
    }

    pub fn chars(&self) -> impl Iterator<Item = char> + '_ {
        let mut pos = 0;
        std::iter::from_fn(move || {
            if pos == self.raw.len() {
                return None;
            }
            Some(decode(self.raw, &mut pos).expect("string scanned before decoding"))
        })
    }
}

impl<'a> Parser<'a> {
    pub fn new(input: &'a str) -> Result<Self> {
        Self::with_limit(input, MAX_INPUT_BYTES)
    }

    pub fn with_limit(input: &'a str, max_bytes: usize) -> Result<Self> {
        if input.len() > max_bytes {
            return Err(ValidationError {
                code: "input_limit",
                offset: max_bytes,
            });
        }
        Ok(Self { input, pos: 0 })
    }

    pub fn error(&self, code: &'static str) -> ValidationError {
        ValidationError {
            code,
            offset: self.pos,
        }
    }

    pub fn ws(&mut self) {
        while self
            .input
            .as_bytes()
            .get(self.pos)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.pos += 1;
        }
    }

    pub fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.input.as_bytes().get(self.pos).copied()
    }

    pub fn take(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub fn expect(&mut self, byte: u8) -> Result<()> {
        if self.take(byte) {
            Ok(())
        } else {
            Err(self.error("syntax"))
        }
    }

    pub fn finish(&mut self) -> Result<()> {
        self.ws();
        if self.pos == self.input.len() {
            Ok(())
        } else {
            Err(self.error("trailing_input"))
        }
    }

    pub fn literal(&mut self, text: &str) -> Result<()> {
        self.ws();
        if !self.input[self.pos..].starts_with(text) {
            return Err(self.error("type"));
        }
        self.pos += text.len();
        Ok(())
    }

    pub fn string(&mut self) -> Result<JsonString<'a>> {
        self.expect(b'"')?;
        let start = self.pos;
        let mut len = 0;
        while let Some(byte) = self.input.as_bytes().get(self.pos) {
            if *byte == b'"' {
                let raw = &self.input[start..self.pos];
                self.pos += 1;
                return Ok(JsonString { raw, len });
            }
            decode(self.input, &mut self.pos).ok_or_else(|| self.error("invalid_string"))?;
            len += 1;
        }
        Err(self.error("unterminated_string"))
    }

    pub fn number(&mut self) -> Result<&'a str> {
        self.ws();
        let start = self.pos;
        if self.byte() == Some(b'-') {
            self.pos += 1;
        }
        match self.byte() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.error("type")),
        }
        if self.byte() == Some(b'.') {
            self.pos += 1;
            self.required_digits()?;
        }
        if matches!(self.byte(), Some(b'e' | b'E')) {
            self.pos += 1;
            let exponent_start = self.pos;
            if matches!(self.byte(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            self.required_digits()?;
            let exponent = self.input[exponent_start..self.pos]
                .parse::<i64>()
                .map_err(|_| self.error("exponent_limit"))?;
            if !(-1_000_000..=1_000_000).contains(&exponent) {
                return Err(self.error("exponent_limit"));
            }
        }
        Ok(&self.input[start..self.pos])
    }

    fn byte(&self) -> Option<u8> {
        self.input.as_bytes().get(self.pos).copied()
    }

    fn digits(&mut self) {
        while self.byte().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
    }

    fn required_digits(&mut self) -> Result<()> {
        let before = self.pos;
        self.digits();
        if before == self.pos {
            Err(self.error("invalid_number"))
        } else {
            Ok(())
        }
    }

    /// Find a value's boundary without materializing it; schema validation is
    /// still mandatory afterwards. Depth is checked before each descent.
    pub fn skip(&mut self, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(self.error("depth_limit"));
        }
        match self.peek() {
            Some(b'"') => {
                self.string()?;
            }
            Some(b'{') => self.skip_container(depth, b'{', b'}')?,
            Some(b'[') => self.skip_container(depth, b'[', b']')?,
            Some(b't') => self.literal("true")?,
            Some(b'f') => self.literal("false")?,
            Some(b'n') => self.literal("null")?,
            _ => {
                self.number()?;
            }
        }
        Ok(())
    }

    fn skip_container(&mut self, depth: usize, open: u8, close: u8) -> Result<()> {
        self.expect(open)?;
        if self.take(close) {
            return Ok(());
        }
        loop {
            if open == b'{' {
                self.string()?;
                self.expect(b':')?;
            }
            self.skip(depth + 1)?;
            if self.take(close) {
                return Ok(());
            }
            self.expect(b',')?;
        }
    }
}

fn hex4(text: &str, pos: &mut usize) -> Option<u32> {
    let bytes = text.as_bytes().get(*pos..pos.checked_add(4)?)?;
    let mut value = 0;
    for byte in bytes {
        value = value * 16 + (*byte as char).to_digit(16)?;
    }
    *pos += 4;
    Some(value)
}

fn decode(text: &str, pos: &mut usize) -> Option<char> {
    let byte = *text.as_bytes().get(*pos)?;
    if byte < 0x20 {
        return None;
    }
    if byte != b'\\' {
        let ch = text.get(*pos..)?.chars().next()?;
        *pos += ch.len_utf8();
        return Some(ch);
    }
    *pos += 1;
    let escape = *text.as_bytes().get(*pos)?;
    *pos += 1;
    match escape {
        b'"' => Some('"'),
        b'\\' => Some('\\'),
        b'/' => Some('/'),
        b'b' => Some('\u{8}'),
        b'f' => Some('\u{c}'),
        b'n' => Some('\n'),
        b'r' => Some('\r'),
        b't' => Some('\t'),
        b'u' => {
            let high = hex4(text, pos)?;
            if (0xd800..=0xdbff).contains(&high) {
                if text.as_bytes().get(*pos..*pos + 2)? != b"\\u" {
                    return None;
                }
                *pos += 2;
                let low = hex4(text, pos)?;
                if !(0xdc00..=0xdfff).contains(&low) {
                    return None;
                }
                char::from_u32(0x10000 + ((high - 0xd800) << 10) + low - 0xdc00)
            } else {
                char::from_u32(high)
            }
        }
        _ => None,
    }
}
