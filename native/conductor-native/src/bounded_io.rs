//! Streaming framing that enforces its limit before growing the output buffer.
use std::io::{self, BufRead};

/// Read a line including its optional newline. EOF after a partial line returns
/// that line. On error, callers must stop or explicitly discard the remainder.
pub fn read_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    maximum: usize,
) -> io::Result<usize> {
    line.clear();
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok(line.len());
        }
        let newline = bytes.iter().position(|&byte| byte == b'\n');
        let count = newline.map_or(bytes.len(), |at| at + 1);
        if count > maximum.saturating_sub(line.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "line exceeds byte limit",
            ));
        }
        line.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if newline.is_some() {
            return Ok(line.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;
    #[test]
    fn frame_boundaries_are_independent_of_buffer_capacity() {
        for capacity in 1..16 {
            let mut input = BufReader::with_capacity(capacity, &b"abc\nxy"[..]);
            let mut line = Vec::new();
            assert_eq!(read_line(&mut input, &mut line, 4).unwrap(), 4);
            assert_eq!(line, b"abc\n");
            assert_eq!(read_line(&mut input, &mut line, 4).unwrap(), 2);
            assert_eq!(line, b"xy");
            assert_eq!(read_line(&mut input, &mut line, 4).unwrap(), 0);
            let mut input = BufReader::with_capacity(capacity, &b"abcd\n"[..]);
            assert_eq!(
                read_line(&mut input, &mut line, 4).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert!(line.len() <= 4);
        }
    }
}
