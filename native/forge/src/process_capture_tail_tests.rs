use super::*;

// Whole-stream reference expression, independent of the production ring,
// segmented decoder and normalization state. No interpreter is invoked.
fn reference(bytes: &[u8], maximum: usize) -> String {
    let decoded = String::from_utf8_lossy(bytes);
    let normalized = decoded.replace("\r\n", "\n").replace('\r', "\n");
    let chars: Vec<_> = normalized.chars().rev().take(maximum).collect();
    chars.into_iter().rev().collect()
}
fn captured(bytes: &[u8], maximum: usize, chunk: usize) -> String {
    let mut tail = TailBuffer::new(maximum).unwrap();
    for bytes in bytes.chunks(chunk) {
        tail.push(bytes);
    }
    assert_eq!(tail.total_bytes(), bytes.len() as u64);
    tail.finish()
}

#[test]
fn explicit_utf8_replacement_and_universal_newline_goldens() {
    let cases: &[(&[u8], usize, &str)] = &[
        (b"", 1, ""),
        (b"abc", 1, "c"),
        (b"abc", 3, "abc"),
        (b"a\r\nb\rc\n", 4, "b\nc\n"),
        (b"\r\r\n", 8, "\n\n"),
        (b"\r\n\r", 1, "\n"),
        (b"a\0b", 3, "a\0b"),
        (b"\xe2\x82X", 2, "\u{fffd}X"),
        (b"\xe2\x82", 5, "\u{fffd}"),
        (b"\xed\xa0\x80", 5, "\u{fffd}\u{fffd}\u{fffd}"),
        (b"\xc0\xaf", 5, "\u{fffd}\u{fffd}"),
        (b"\xf0\x28\x8c\xbc", 8, "\u{fffd}(\u{fffd}\u{fffd}"),
        (b"\xf4\x90\x80\x80", 8, "\u{fffd}\u{fffd}\u{fffd}\u{fffd}"),
        (b"\xe2\x82\xff", 8, "\u{fffd}\u{fffd}"),
        (b"\xf0\x9f\x92", 1, "\u{fffd}"),
        ("e\u{301}".as_bytes(), 1, "\u{301}"),
        ("界💡é".as_bytes(), 2, "💡é"),
        ("a\u{2028}b".as_bytes(), 3, "a\u{2028}b"),
    ];
    for &(bytes, maximum, expected) in cases {
        assert_eq!(
            reference(bytes, maximum),
            expected,
            "reference mismatch for {bytes:?}"
        );
        for chunk in [1, 2, 3, 4, 7, 64] {
            assert_eq!(
                captured(bytes, maximum, chunk),
                expected,
                "chunk={chunk}, bytes={bytes:?}"
            );
        }
    }
}

#[test]
fn normalization_precedes_scalar_truncation_even_for_n_one() {
    for (bytes, maximum, expected) in [
        (b"head\r\n".as_slice(), 1, "\n"),
        (b"head\r\n".as_slice(), 2, "d\n"),
        (b"head\r".as_slice(), 2, "d\n"),
        (b"a\r\r\nb".as_slice(), 3, "\n\nb"),
    ] {
        assert_eq!(captured(bytes, maximum, 1), expected);
    }
    let input = "💡".repeat(64);
    assert_eq!(captured(input.as_bytes(), 1, 3), "💡");
    let input = "\r\n".repeat(64);
    assert_eq!(captured(input.as_bytes(), 1, 1), "\n");
    assert_eq!(captured(input.as_bytes(), 2, 7), "\n\n");
}

#[test]
fn every_two_chunk_split_and_ring_boundary_matches_whole_stream_contract() {
    let mut bytes = vec![b'p'; 257];
    bytes.extend_from_slice("A💡界é\r\nB\rC\n".as_bytes());
    bytes.extend_from_slice(b"\xe2\x82X\xed\xa0\x80\r\n\xf0\x9f\x92");
    for maximum in 1..=24 {
        let expected = reference(&bytes, maximum);
        for split in 0..=bytes.len() {
            let mut tail = TailBuffer::new(maximum).unwrap();
            tail.push(&bytes[..split]);
            tail.push(&[]);
            tail.push(&bytes[split..]);
            assert_eq!(tail.total_bytes(), bytes.len() as u64);
            assert_eq!(tail.finish(), expected, "maximum={maximum}, split={split}");
        }
    }
}

#[test]
fn deterministic_binary_chunks_cover_invalid_utf8_grouping_and_partial_eof() {
    let mut state = 0x3ca5_7f92_u32;
    let mut bytes = Vec::with_capacity(4096);
    for _ in 0..4096 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        bytes.push((state >> 24) as u8);
    }
    for maximum in 1..=32 {
        let expected = reference(&bytes, maximum);
        for chunk in [1, 2, 3, 7, 64, 1024, 4096] {
            assert_eq!(
                captured(&bytes, maximum, chunk),
                expected,
                "maximum={maximum}, chunk={chunk}"
            );
        }
    }
}

#[test]
fn push_keeps_fixed_retention_allocation_and_counts_every_discarded_byte() {
    let mut tail = TailBuffer::new(32).unwrap();
    let pointer = tail.bytes.as_ptr();
    let capacity = 4 * 32 + 3;
    assert_eq!(tail.bytes.len(), capacity);
    let large = vec![b'x'; 1024 * 1024];
    tail.push(&large);
    for _ in 0..1000 {
        tail.push(b"ab\r\n");
    }
    assert_eq!(tail.bytes.as_ptr(), pointer);
    assert_eq!(tail.bytes.len(), capacity);
    assert!(tail.retained <= capacity);
    assert_eq!(tail.total_bytes(), (large.len() + 4000) as u64);
    let expected = "ab\n".repeat(1000);
    assert_eq!(tail.finish(), reference(expected.as_bytes(), 32));
}

#[test]
fn admission_and_saturating_production_counter_are_explicit() {
    for invalid in [0, MAX_OUTPUT_CHARS + 1, usize::MAX] {
        assert!(TailBuffer::new(invalid).is_err());
    }
    let maximum = TailBuffer::new(MAX_OUTPUT_CHARS).unwrap();
    assert_eq!(maximum.bytes.len(), 4 * MAX_OUTPUT_CHARS + 3);
    let mut tail = TailBuffer::new(2).unwrap();
    tail.total = u64::MAX - 1;
    tail.push(b"abcd");
    tail.push(&[]);
    assert_eq!(tail.total_bytes(), u64::MAX);
    assert_eq!(tail.finish(), "cd");
}
