use super::*;
fn identity(inode: u64) -> Identity {
    Identity {
        device: 1,
        inode,
        mode: 0o100600,
        length: 1,
        sha256: "a".repeat(64),
    }
}
fn begin() -> Event {
    Event::Begin {
        root: "/repo".into(),
        root_device: 1,
        root_inode: 2,
        common_dir: "/repo/.git".into(),
        common_device: 1,
        common_inode: 3,
        index_path: "/repo/.git/index".into(),
        index_parent_device: 1,
        index_parent_inode: 3,
        original_index: None,
        recovery_oid: "1".repeat(40),
        branch: "refs/heads/main".into(),
        old: "1".repeat(40),
        new: "2".repeat(40),
        target: "refs/remotes/origin/main".into(),
        recovery_ref: "refs/snapshots/forge/test".into(),
    }
}
fn depot() -> [Event; 2] {
    [
        Event::BeforeDepot {
            name: "checkout-sync-depot-test".into(),
        },
        Event::AfterDepot {
            name: "checkout-sync-depot-test".into(),
            device: 1,
            inode: 5,
        },
    ]
}
fn path(name: &str) -> Event {
    Event::BeforePath {
        path: name.into(),
        parent_device: 1,
        parent_inode: 4,
        slot: format!("checkout-sync-slot-{name}"),
        original: Some(identity(1)),
        original_blob: Some(format!("checkout-sync-slot-{name}")),
        staged: Some(identity(2)),
    }
}
fn stage(name: &str) -> [Event; 2] {
    [
        Event::BeforeStage {
            path: name.into(),
            parent_device: 1,
            parent_inode: 4,
            slot: format!("checkout-sync-slot-{name}"),
            original: Some(identity(1)),
            original_blob: Some(format!("checkout-sync-slot-{name}")),
            has_staged: true,
        },
        Event::AfterStage {
            path: name.into(),
            staged: Some(identity(2)),
        },
    ]
}
#[test]
fn phase_parser_retains_incomplete_intents_and_enforces_reverse_recovery() {
    let mut events = vec![begin()];
    events.extend(depot());
    events.extend(stage("a"));
    events.extend(stage("b"));
    events.extend([
        path("a"),
        Event::AfterPath {
            path: "a".into(),
            published: Some(identity(2)),
        },
        path("b"),
    ]);
    assert_eq!(subjects(&parse(&events).unwrap()), ["path:b", "path:a"]);
    events.push(Event::BeforeRollback {
        subject: "path:a".into(),
    });
    assert!(parse(&events).is_err());
    events.pop();
    events.extend([
        Event::GuardAcquire {
            subject: "refs".into(),
        },
        Event::GuardHeld {
            subject: "refs".into(),
        },
        Event::GuardAcquire {
            subject: "index".into(),
        },
        Event::GuardHeld {
            subject: "index".into(),
        },
    ]);
    events.push(Event::BeforeRollback {
        subject: "path:b".into(),
    });
    events.push(Event::BeforeRollback {
        subject: "path:b".into(),
    });
    events.push(Event::Rollback {
        subject: "path:b".into(),
        restored: true,
        detail: "done".into(),
    });
    assert!(parse(&events).unwrap().restored.contains("path:b"));
    events.push(Event::Rollback {
        subject: "path:a".into(),
        restored: true,
        detail: "unmarked".into(),
    });
    assert!(parse(&events).is_err());
}
#[test]
fn completion_requires_exact_publication_records() {
    let mut events = vec![begin(), Event::Complete];
    assert!(parse(&events).is_err());
    events = vec![
        begin(),
        Event::BeforeIndex {
            parent_device: 1,
            parent_inode: 3,
            original: None,
            original_blob: None,
            staged: identity(3),
        },
        Event::AfterIndex {
            published: identity(3),
        },
        Event::BeforeRef {
            branch: "refs/heads/main".into(),
            old: "1".repeat(40),
            new: "2".repeat(40),
        },
        Event::AfterRef {
            branch: "refs/heads/main".into(),
            new: "2".repeat(40),
        },
        Event::Complete,
    ];
    assert!(parse(&events).unwrap().completed);
    events.push(Event::BeforeRollback {
        subject: "ref".into(),
    });
    assert!(parse(&events).is_err());
}
#[test]
fn guard_phases_reject_unprotected_rollback_and_terminal_recovery() {
    let mut events = vec![begin()];
    events.extend(depot());
    events.extend(stage("a"));
    events.push(path("a"));
    let mut unprotected = events.clone();
    unprotected.push(Event::BeforeRollback {
        subject: "path:a".into(),
    });
    assert!(parse(&unprotected).is_err());
    events.push(Event::GuardAcquire {
        subject: "refs".into(),
    });
    assert_eq!(
        parse(&events).unwrap().guard_states.get("refs").unwrap(),
        "acquiring"
    );
    let mut bad = events.clone();
    bad.push(Event::GuardAcquire {
        subject: "index".into(),
    });
    assert!(parse(&bad).is_err());
    events.push(Event::GuardHeld {
        subject: "refs".into(),
    });
    events.push(Event::GuardAcquire {
        subject: "index".into(),
    });
    events.push(Event::GuardHeld {
        subject: "index".into(),
    });
    events.push(Event::BeforeRollback {
        subject: "path:a".into(),
    });
    events.push(Event::Rollback {
        subject: "path:a".into(),
        restored: true,
        detail: "done".into(),
    });
    let mut crash = events.clone();
    crash.push(Event::RecoveryComplete);
    assert!(parse(&crash).is_err());
    for event in [
        Event::GuardRelease {
            subject: "index".into(),
        },
        Event::GuardReleased {
            subject: "index".into(),
        },
        Event::GuardRelease {
            subject: "refs".into(),
        },
        Event::GuardReleased {
            subject: "refs".into(),
        },
        Event::RecoveryComplete,
    ] {
        events.push(event);
    }
    assert!(parse(&events).unwrap().recovered);
}
#[test]
fn staging_after_publication_is_valid_but_unpaired_stage_is_uncertain() {
    let mut events = vec![begin()];
    events.extend(depot());
    events.extend(stage("a"));
    events.push(path("a"));
    events.push(Event::AfterPath {
        path: "a".into(),
        published: Some(identity(2)),
    });
    events.extend(stage("b"));
    assert!(parse(&events).is_ok());
    let mut events = vec![begin()];
    events.extend(depot());
    events.push(stage("a")[0].clone());
    assert!(parse(&events).unwrap().incomplete_setup.is_some());
}

#[test]
fn forward_snapshot_intent_is_bound_and_guarded_before_publication() {
    let mut start = begin();
    if let Event::Begin { recovery_oid, .. } = &mut start {
        recovery_oid.clear();
    }
    let mut events = vec![start];
    let intent = Event::BeforeSnapshot {
        reference: "refs/snapshots/forge/test".into(),
        oid: "3".repeat(40),
    };
    let mut unsafe_events = events.clone();
    unsafe_events.push(intent.clone());
    assert!(parse(&unsafe_events).is_err());
    events.extend([
        Event::ForwardAcquire {
            subject: "refs".into(),
        },
        Event::ForwardHeld {
            subject: "refs".into(),
        },
        Event::ForwardAcquire {
            subject: "index".into(),
        },
        Event::ForwardHeld {
            subject: "index".into(),
        },
        intent,
    ]);
    assert_eq!(
        parse(&events).unwrap().snapshot_pending,
        Some("3".repeat(40))
    );
    let mut mismatch = events.clone();
    mismatch.push(Event::SnapshotReady {
        oid: "4".repeat(40),
    });
    assert!(parse(&mismatch).is_err());
    events.push(Event::SnapshotReady {
        oid: "3".repeat(40),
    });
    assert_eq!(parse(&events).unwrap().header.recovery_oid, "3".repeat(40));
    let mut wrong_release = events.clone();
    wrong_release.push(Event::ForwardRelease {
        subject: "refs".into(),
    });
    assert!(parse(&wrong_release).is_err());
}
#[test]
fn interrupted_forward_guard_phases_are_not_complete() {
    for state in 0..3 {
        let mut events = vec![
            begin(),
            Event::ForwardAcquire {
                subject: "refs".into(),
            },
        ];
        if state >= 1 {
            events.push(Event::ForwardHeld {
                subject: "refs".into(),
            });
        }
        if state >= 2 {
            events.push(Event::ForwardRelease {
                subject: "refs".into(),
            });
        }
        assert_ne!(
            parse(&events).unwrap().forward_guards.get("refs").unwrap(),
            "released"
        );
        events.push(Event::RecoveryComplete);
        assert!(parse(&events).is_err());
    }
}

#[test]
fn ref_intent_requires_reacquired_index_guard() {
    let mut events = vec![begin()];
    for event in [
        Event::ForwardAcquire {
            subject: "refs".into(),
        },
        Event::ForwardHeld {
            subject: "refs".into(),
        },
        Event::ForwardAcquire {
            subject: "index".into(),
        },
        Event::ForwardHeld {
            subject: "index".into(),
        },
        Event::BeforeIndex {
            parent_device: 1,
            parent_inode: 3,
            original: None,
            original_blob: None,
            staged: identity(3),
        },
        Event::ForwardRelease {
            subject: "index".into(),
        },
        Event::ForwardReleased {
            subject: "index".into(),
        },
        Event::AfterIndex {
            published: identity(3),
        },
    ] {
        events.push(event);
    }
    let intent = Event::BeforeRef {
        branch: "refs/heads/main".into(),
        old: "1".repeat(40),
        new: "2".repeat(40),
    };
    let mut missing = events.clone();
    missing.push(intent.clone());
    assert!(parse(&missing).is_err());
    events.push(Event::ForwardAcquire {
        subject: "index".into(),
    });
    let mut acquiring = events.clone();
    acquiring.push(intent.clone());
    assert!(parse(&acquiring).is_err());
    events.push(Event::ForwardHeld {
        subject: "index".into(),
    });
    events.push(intent);
    assert!(parse(&events).is_ok());
    events.push(Event::ForwardRelease {
        subject: "refs".into(),
    });
    events.push(Event::ForwardReleased {
        subject: "refs".into(),
    });
    events.push(Event::AfterRef {
        branch: "refs/heads/main".into(),
        new: "2".repeat(40),
    });
    let mut held = events.clone();
    held.push(Event::Complete);
    assert!(parse(&held).is_err());
    events.push(Event::ForwardRelease {
        subject: "index".into(),
    });
    events.push(Event::ForwardReleased {
        subject: "index".into(),
    });
    events.push(Event::Complete);
    assert!(parse(&events).unwrap().completed);
}

#[test]
fn rollback_order_places_ref_after_index_and_paths() {
    let mut events = vec![begin()];
    events.extend(depot());
    events.extend(stage("a"));
    events.push(path("a"));
    events.push(Event::AfterPath {
        path: "a".into(),
        published: Some(identity(2)),
    });
    events.push(Event::BeforeIndex {
        parent_device: 1,
        parent_inode: 3,
        original: None,
        original_blob: None,
        staged: identity(3),
    });
    events.push(Event::AfterIndex {
        published: identity(3),
    });
    events.push(Event::BeforeRef {
        branch: "refs/heads/main".into(),
        old: "1".repeat(40),
        new: "2".repeat(40),
    });
    assert_eq!(
        subjects(&parse(&events).unwrap()),
        ["index", "path:a", "ref"]
    );
    events.push(Event::GuardAcquire {
        subject: "refs".into(),
    });
    events.push(Event::GuardHeld {
        subject: "refs".into(),
    });
    let mut early_ref = events.clone();
    early_ref.push(Event::BeforeRollback {
        subject: "ref".into(),
    });
    assert!(parse(&early_ref).is_err());
}
