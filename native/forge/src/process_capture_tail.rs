//! Pure per-stream output tails. No descriptor, process, signal or supervisor.
//! Raw retention is exactly4*N+3 bytes: a normalized scalar consumes at most4
//! UTF8 bytes (CRLF consumes2); the extra3 cover a cut UTF8 prefix. Therefore
//! artificial replacement characters at the retained start cannot reach the
//! lastN normalized Unicode scalars. Incomplete EOF input uses replacement.
use anyhow::{ensure, Context, Result};

pub const MAX_OUTPUT_CHARS: usize = 262_144;
pub struct TailBuffer {
    bytes: Vec<u8>,
    start: usize,
    retained: usize,
    maximum: usize,
    total: u64,
}
impl TailBuffer {
    pub fn new(max_chars: usize) -> Result<Self> {
        ensure!(
            (1..=MAX_OUTPUT_CHARS).contains(&max_chars),
            "tail character limit must be 1..262144"
        );
        let capacity = max_chars
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(3))
            .context("tail byte retention overflow")?;
        Ok(Self {
            bytes: vec![0; capacity],
            start: 0,
            retained: 0,
            maximum: max_chars,
            total: 0,
        })
    }
    /// Accept every byte presented by the shared draining loop. Once admitted,
    /// retention cannot fail, grow its allocation, stop reading or signal a child.
    pub fn push(&mut self, bytes: &[u8]) {
        self.total = self
            .total
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        let capacity = self.bytes.len();
        if bytes.len() >= capacity {
            self.bytes.copy_from_slice(&bytes[bytes.len() - capacity..]);
            self.start = 0;
            self.retained = capacity;
            return;
        }
        let drop = (self.retained + bytes.len()).saturating_sub(capacity);
        self.start = (self.start + drop) % capacity;
        self.retained -= drop;
        let end = (self.start + self.retained) % capacity;
        let first = bytes.len().min(capacity - end);
        self.bytes[end..end + first].copy_from_slice(&bytes[..first]);
        self.bytes[..bytes.len() - first].copy_from_slice(&bytes[first..]);
        self.retained += bytes.len();
    }
    pub fn total_bytes(&self) -> u64 {
        self.total
    }
    /// UTF8 errors='replace', then universal-newline normalization, then lastN
    /// Unicode scalars. Decoding happens only here with linear retained-byte work.
    pub fn finish(mut self) -> String {
        if self.start != 0 {
            self.bytes.rotate_left(self.start);
        }
        self.bytes.truncate(self.retained);
        normalized_tail(&self.bytes, self.maximum)
    }
}
struct Scalars {
    values: Vec<char>,
    start: usize,
    len: usize,
    utf8_bytes: usize,
    previous_cr: bool,
}
impl Scalars {
    fn new(maximum: usize) -> Self {
        Self {
            values: vec!['\0'; maximum],
            start: 0,
            len: 0,
            utf8_bytes: 0,
            previous_cr: false,
        }
    }
    fn emit(&mut self, character: char) {
        if self.previous_cr && character == '\n' {
            self.previous_cr = false;
            return;
        }
        self.previous_cr = character == '\r';
        let character = if self.previous_cr { '\n' } else { character };
        let maximum = self.values.len();
        if self.len == maximum {
            self.utf8_bytes -= self.values[self.start].len_utf8();
            self.values[self.start] = character;
            self.start = (self.start + 1) % maximum;
        } else {
            self.values[(self.start + self.len) % maximum] = character;
            self.len += 1;
        }
        self.utf8_bytes += character.len_utf8();
    }
    fn finish(self) -> String {
        let mut text = String::with_capacity(self.utf8_bytes);
        for index in 0..self.len {
            text.push(self.values[(self.start + index) % self.values.len()]);
        }
        text
    }
}
fn normalized_tail(mut bytes: &[u8], maximum: usize) -> String {
    let mut scalars = Scalars::new(maximum);
    // Each iteration consumes a valid prefix plus at least one invalid byte,
    // or finishes. No retained prefix is revisited on the next iteration.
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                for character in valid.chars() {
                    scalars.emit(character);
                }
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                let prefix = std::str::from_utf8(&bytes[..valid])
                    .expect("UTF8 error exposes a valid prefix");
                for character in prefix.chars() {
                    scalars.emit(character);
                }
                let invalid = error.error_len().unwrap_or(bytes.len() - valid);
                scalars.emit('\u{fffd}');
                bytes = &bytes[valid + invalid..];
            }
        }
    }
    scalars.finish()
}

#[cfg(test)]
#[path = "process_capture_tail_tests.rs"]
mod tests;
