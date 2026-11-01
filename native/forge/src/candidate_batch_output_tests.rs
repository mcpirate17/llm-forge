use super::*;

#[test]
fn encoded_output_limit_counts_json_escaping_and_unicode_exactly() {
    let value = json!({"path":"a\n\u{0000}\"🦀é", "items":[1,false,null]});
    let encoded = serde_json::to_vec(&value).unwrap();
    assert_eq!(bytes(&value, encoded.len()).unwrap(), encoded.len());
    assert!(bytes(&value, encoded.len() - 1).is_err());
    assert!(bytes(&json!(null), 0).is_err());
}

#[test]
fn concurrent_output_admission_never_overdraws_and_failed_reserve_is_atomic() {
    let quota = Arc::new(Quota {
        remaining: AtomicUsize::new(1000),
    });
    assert!(quota.reserve(1001).is_err());
    let admitted = std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..16)
            .map(|_| {
                let quota = quota.clone();
                scope.spawn(move || quota.reserve(200).is_ok())
            })
            .collect();
        tasks
            .into_iter()
            .map(|task| usize::from(task.join().unwrap()))
            .sum::<usize>()
    });
    assert_eq!(admitted, 5);
    assert_eq!(quota.remaining.load(Ordering::Acquire), 0);
    assert!(quota.reserve(1).is_err());
    quota.reserve(0).unwrap();
}
