use super::*;
use crate::confined_execution::{Outcome, Terminal};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[tokio::test(flavor = "current_thread")]
async fn outputs_follow_input_order_despite_out_of_order_completion() {
    let batch = start(vec![0, 1, 2, 3], 3, |job, _, _| async move {
        tokio::time::sleep(Duration::from_millis((3 - job) * 5)).await;
        Ok(job)
    })
    .unwrap();
    let results = tokio::time::timeout(Duration::from_secs(2), batch.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        results.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn panic_cancels_active_jobs_and_rejects_queued_jobs() {
    let started = Arc::new(AtomicUsize::new(0));
    let count = started.clone();
    let batch = start(vec![0, 1, 2, 3], 2, move |job, cancel, _| {
        let count = count.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            if job == 1 {
                panic!("injected worker panic");
            }
            cancel.cancelled().await?;
            Ok(job)
        }
    })
    .unwrap();
    let results = tokio::time::timeout(Duration::from_secs(2), batch.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(results[1].is_err() && results[2].is_err() && results[3].is_err());
    assert!(started.load(Ordering::SeqCst) <= 2);
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_batch_cancels_active_and_owner_survives() {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let batch = start(vec![0], 1, move |_, cancel, _| {
        let sender = sender.clone();
        async move {
            sender.send("started").unwrap();
            cancel.cancelled().await?;
            sender.send("cancelled").unwrap();
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(receiver.recv().await, Some("started"));
    drop(batch);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap(),
        Some("cancelled")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_is_level_triggered_for_all_waiters() {
    let cancel = Arc::new(Cancellation::new().unwrap());
    let a = cancel.clone();
    let b = cancel.clone();
    let first = tokio::spawn(async move { a.cancelled().await });
    let second = tokio::spawn(async move { b.cancelled().await });
    tokio::task::yield_now().await;
    cancel.cancel().unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
    })
    .await
    .unwrap();
    cancel.cancelled().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_guard_retains_terminal_failure_and_closed_watch_is_error() {
    let owners = Arc::new(Owners::new(Arc::new(Cancellation::new().unwrap())));
    let cancel = Arc::new(Cancellation::new().unwrap());
    let (send, completion) = Completion::test_channel();
    owners.inner.lock().unwrap().entries.insert(
        0,
        Entry {
            completion,
            cancel: cancel.clone(),
            dropped: false,
        },
    );
    drop(Guard {
        owners: owners.clone(),
        id: 0,
        finished: false,
    });
    assert!(cancel.is_cancelled());
    send.send_replace(Some(Terminal {
        outcome: Outcome::Failed,
        cleanup_complete: false,
    }));
    {
        let mut registry = owners.inner.lock().unwrap();
        prune_dropped(&mut registry);
        assert!(registry.entries.is_empty());
    }
    assert!(owners.finish_all().await.is_err());
    let (send, mut completion) = Completion::test_channel();
    drop(send);
    assert!(completion.wait().await.is_err());
}

#[test]
fn signal_mask_can_cross_worker_threads() {
    fn assert_send<T: Send + Copy>() {}
    assert_send::<crate::process_signals::ChildSignalMask>();
}

#[tokio::test(flavor = "current_thread")]
async fn poisoned_registry_cancels_known_owner_and_waits_for_terminal() {
    let owners = Arc::new(Owners::new(Arc::new(Cancellation::new().unwrap())));
    let cancellation = Arc::new(Cancellation::new().unwrap());
    let (sender, completion) = Completion::test_channel();
    owners.inner.lock().unwrap().entries.insert(
        0,
        Entry {
            completion,
            cancel: cancellation.clone(),
            dropped: true,
        },
    );
    let poisoned = owners.clone();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _guard = poisoned.inner.lock().unwrap();
        panic!("inject poisoned registry");
    }));
    let task_owners = owners.clone();
    let task = tokio::spawn(async move { task_owners.finish_all().await });
    tokio::time::timeout(Duration::from_secs(2), cancellation.cancelled())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !task.is_finished(),
        "cleanup returned before terminal evidence"
    );
    sender.send_replace(Some(Terminal {
        outcome: Outcome::Cancelled,
        cleanup_complete: true,
    }));
    let error = task.await.unwrap().unwrap_err();
    assert!(format!("{error:#}").contains("poisoned"));
}

#[tokio::test(flavor = "current_thread")]
async fn outer_cleanup_failure_retains_bounded_job_error_context() {
    let batch = start(vec![0, 1], 2, |job, _, owners| async move {
        if job == 0 {
            anyhow::bail!("workspace retained at /tmp/important-recovery");
        }
        let _lock = owners.inner.lock().unwrap();
        panic!("poison owner registry");
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    })
    .unwrap();
    let error = batch.wait().await.unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("poisoned"), "{message}");
    assert!(message.contains("/tmp/important-recovery"), "{message}");
}
