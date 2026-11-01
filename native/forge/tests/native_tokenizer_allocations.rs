#[path = "support/allocation_counter.rs"]
mod allocation_counter;
#[path = "support/tokenizer_fixture.rs"]
mod fixture;
use forge::tokenizer::Counter;
use std::hint::black_box;

#[test]
fn repeated_successful_hash_count_hits_allocate_nothing() {
    let fixture = fixture::Fixture::new();
    let counter = Counter::load(&fixture.config).unwrap();
    let text = "hello world! 東京 🦀\n".repeat(2048);
    let expected = counter.count(&text).unwrap();
    let (correct, allocations) = allocation_counter::measure(|| {
        (0..100).all(|_| black_box(counter.count(black_box(&text))).unwrap() == expected)
    });
    assert!(correct);
    assert_eq!(allocations, 0);
}
