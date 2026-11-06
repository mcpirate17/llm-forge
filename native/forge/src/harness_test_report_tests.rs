use super::*;

#[test]
fn validates_report_envelope_and_output_budget() {
    let root = Path::new("/fixture");
    let mut value = json!({"schema_version":1,"authority":"native-static-analysis",
        "advisory":true,"root":"/fixture","report":{"broken":[]}});
    let text = value.to_string();
    assert_eq!(envelope(&text, root, text.len()).unwrap(), value);
    assert!(envelope(&text, root, text.len() - 1).is_err());
    for (key, changed) in [
        ("root", json!("/elsewhere")),
        ("advisory", json!(false)),
        ("schema_version", json!(2)),
        ("authority", json!("external")),
        ("report", json!(null)),
    ] {
        let previous = value[key].clone();
        value[key] = changed;
        assert!(envelope(&value.to_string(), root, 4096).is_err(), "{key}");
        value[key] = previous;
    }
    assert!(envelope("not json", root, 4096).is_err());
}

#[test]
fn worker_failure_keeps_exit_and_bounded_utf8_diagnostic() {
    let stderr = format!("bad config {}", "界".repeat(1000));
    let text = worker_failure(Outcome::Exited, Some(2), &stderr);
    assert!(text.contains("exit_code=Some(2)"));
    assert!(text.contains("worker stderr (untrusted"));
    let captured = text.split_once(": bad config ").unwrap().1;
    assert!(captured.len() <= 2048);
    assert!(captured.len() < stderr.len());
}

#[tokio::test]
async fn cancellation_watches_observe_preexisting_and_later_cancellation() {
    for index in 0..3 {
        let (a, mut ar) = watch::channel(false);
        let (b, mut br) = watch::channel(false);
        let (c, mut cr) = watch::channel(false);
        let senders = [a, b, c];
        senders[index].send_replace(true);
        tokio::time::timeout(
            Duration::from_millis(100),
            cancelled(&mut ar, &mut br, &mut cr),
        )
        .await
        .expect("preexisting cancellation must be observed");
    }
    let (a, mut ar) = watch::channel(false);
    let (_b, mut br) = watch::channel(false);
    let (_c, mut cr) = watch::channel(false);
    let task = tokio::spawn(async move { cancelled(&mut ar, &mut br, &mut cr).await });
    a.send_replace(true);
    tokio::time::timeout(Duration::from_millis(100), task)
        .await
        .unwrap()
        .unwrap();
}
