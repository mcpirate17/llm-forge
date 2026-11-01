use forge::terminal_buffer::ReplayBuffer;

#[test]
fn replay_matches_an_independent_append_only_stream_through_wraps_and_evictions() {
    for capacity in [1, 17, 257, 1024] {
        let mut replay = ReplayBuffer::new(capacity).unwrap();
        let mut original = Vec::new();
        for step in 0..1000 {
            let input: Vec<u8> = (0..(step * 73) % 301)
                .map(|i| ((i + step) % 256) as u8)
                .collect();
            replay.append(&input).unwrap();
            original.extend_from_slice(&input);
            let earliest = original.len().saturating_sub(capacity);
            assert_eq!(replay.retained_start(), earliest as u64);
            assert_eq!(replay.end(), original.len() as u64);
            for cursor in [
                0,
                earliest,
                original.len().saturating_sub(37),
                original.len(),
            ] {
                let limit = step % 64 + 1;
                let actual = replay.read(cursor as u64, limit).unwrap();
                let begin = cursor.max(earliest);
                let end = original.len().min(begin + limit);
                assert_eq!(actual.bytes, original[begin..end]);
                assert_eq!(actual.start, begin as u64);
                assert_eq!(actual.next, end as u64);
                assert_eq!(actual.end, original.len() as u64);
                assert_eq!(actual.evicted, cursor < earliest);
            }
        }
    }
}

#[test]
fn empty_data_invalid_bounds_and_multi_client_offsets_are_explicit() {
    assert!(ReplayBuffer::new(0).is_err());
    assert!(ReplayBuffer::new(4 * 1024 * 1024 + 1).is_err());
    let mut replay = ReplayBuffer::new(8).unwrap();
    assert!(replay.read(0, 0).is_err());
    assert!(replay.read(0, 65537).is_err());
    assert!(replay.read(1, 1).is_err());
    assert!(replay.read(0, 1).unwrap().bytes.is_empty());
    replay.append(b"abcdef").unwrap();
    let first = replay.read(0, 3).unwrap();
    let second = replay.read(0, 2).unwrap();
    replay.append(b"ghij").unwrap();
    assert_eq!(replay.read(first.next, 64).unwrap().bytes, b"defghij");
    assert_eq!(replay.read(second.next, 64).unwrap().bytes, b"cdefghij");
    replay.append(b"").unwrap();
    assert_eq!(replay.retained_start(), 2);
    assert_eq!(replay.end(), 10);
}
