use super::*;

fn fixture() -> (tempfile::TempDir, SourceRoot, Snapshot, Snapshot) {
    let directory = tempfile::tempdir().unwrap();
    git2::Repository::init(directory.path()).unwrap();
    std::fs::write(directory.path().join("first.txt"), "original first").unwrap();
    std::fs::write(directory.path().join("second.txt"), "original second").unwrap();
    let root = SourceRoot::open(directory.path()).unwrap();
    let first = root.read(Path::new("first.txt")).unwrap();
    let second = root.read(Path::new("second.txt")).unwrap();
    (directory, root, first, second)
}

#[test]
fn later_publish_failure_restores_prior_publication_and_retains_recovery() {
    let (_directory, root, first, second) = fixture();
    let error = root
        .replace_many_inner(
            &[(&first, "candidate first"), (&second, "candidate second")],
            || Ok(()),
            |index| {
                ensure!(index != 1, "injected second publication failure");
                Ok(())
            },
        )
        .unwrap_err();
    assert_eq!(root.read(first.path()).unwrap().content(), first.content());
    assert_eq!(
        root.read(second.path()).unwrap().content(),
        second.content()
    );
    let error = format!("{error:#}");
    assert!(error.contains("injected second publication failure"));
    for original in [&first, &second] {
        let id = root.git_snapshot(original.content()).unwrap();
        assert!(error.contains(&format!("{}={id}", original.path().display())));
        assert_eq!(root.recovery_blob(&id).unwrap(), original.content());
    }
}

#[test]
fn rollback_preserves_concurrent_in_place_edit() {
    let (directory, root, first, second) = fixture();
    let error = root
        .replace_many_inner(
            &[(&first, "candidate first"), (&second, "candidate second")],
            || Ok(()),
            |index| {
                if index == 1 {
                    std::fs::write(directory.path().join(first.path()), "concurrent bytes")?;
                    anyhow::bail!("injected later publication failure");
                }
                Ok(())
            },
        )
        .unwrap_err();
    assert_eq!(
        root.read(first.path()).unwrap().content(),
        "concurrent bytes"
    );
    assert_eq!(
        root.read(second.path()).unwrap().content(),
        second.content()
    );
    let error = format!("{error:#}");
    assert!(error.contains("first.txt: concurrent edit or replacement prevents rollback"));
    assert!(error.contains("first.txt="));
    assert!(error.contains("second.txt="));
}

#[test]
fn rollback_preserves_concurrent_inode_with_identical_candidate_bytes() {
    let (directory, root, first, second) = fixture();
    let error = root
        .replace_many_inner(
            &[(&first, "candidate first"), (&second, "candidate second")],
            || Ok(()),
            |index| {
                if index == 1 {
                    let external = directory.path().join("external.txt");
                    std::fs::write(&external, "candidate first")?;
                    std::fs::rename(external, directory.path().join(first.path()))?;
                    anyhow::bail!("injected later publication failure");
                }
                Ok(())
            },
        )
        .unwrap_err();
    assert_eq!(
        root.read(first.path()).unwrap().content(),
        "candidate first"
    );
    assert!(format!("{error:#}").contains("rollback incomplete"));
}

#[test]
fn validator_runs_under_lock_and_changed_source_aborts_before_publication() {
    let (directory, root, first, second) = fixture();
    let error = root
        .replace_many(
            &[(&first, "candidate first"), (&second, "candidate second")],
            || {
                let nested = root.replace_many(&[(&second, "nested")], || Ok(()));
                assert!(format!("{:#}", nested.unwrap_err()).contains("root lock"));
                std::fs::write(directory.path().join(second.path()), "concurrent second")?;
                Ok(())
            },
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("source changed"));
    assert_eq!(root.read(first.path()).unwrap().content(), first.content());
    assert_eq!(
        root.read(second.path()).unwrap().content(),
        "concurrent second"
    );
}

#[test]
fn content_preparation_and_batch_reports_support_empty_content() {
    use crate::patch;
    let (directory, root, first, second) = fixture();
    std::fs::write(directory.path().join("empty.txt"), "").unwrap();
    let empty = root.read(Path::new("empty.txt")).unwrap();
    let plans = vec![
        patch::prepare_content(&root, first.path(), first.sha256(), "", None).unwrap(),
        patch::prepare_content(&root, empty.path(), empty.sha256(), "new", None).unwrap(),
        patch::prepare_content(&root, second.path(), second.sha256(), "changed", None).unwrap(),
    ];
    let reports = patch::apply_many(&root, plans, || Ok(())).unwrap();
    assert_eq!(reports.len(), 3);
    assert!(reports
        .iter()
        .all(|report| report.applied && report.git_snapshot.is_some()));
    assert_eq!(root.read(first.path()).unwrap().content(), "");
    assert_eq!(root.read(empty.path()).unwrap().content(), "new");
}

#[test]
fn rollback_continues_past_conflict_to_restore_other_files() {
    let (directory, root, first, second) = fixture();
    std::fs::write(directory.path().join("third.txt"), "original third").unwrap();
    let third = root.read(Path::new("third.txt")).unwrap();
    let error = root
        .replace_many_inner(
            &[
                (&first, "candidate first"),
                (&second, "candidate second"),
                (&third, "candidate third"),
            ],
            || Ok(()),
            |index| {
                if index == 2 {
                    std::fs::write(directory.path().join(second.path()), "concurrent second")?;
                    anyhow::bail!("injected third publication failure");
                }
                Ok(())
            },
        )
        .unwrap_err();
    assert_eq!(root.read(first.path()).unwrap().content(), first.content());
    assert_eq!(
        root.read(second.path()).unwrap().content(),
        "concurrent second"
    );
    assert_eq!(root.read(third.path()).unwrap().content(), third.content());
    assert!(format!("{error:#}").contains("second.txt: concurrent edit"));
}

#[test]
fn publication_monitor_failure_after_rename_rolls_back_and_keeps_polling_cleanup() {
    let (_directory, root, first, second) = fixture();
    let mut polls = 0;
    let error = root
        .replace_many_monitored(
            &[(&first, "candidate first"), (&second, "candidate second")],
            || Ok(()),
            || {
                polls += 1;
                ensure!(polls != 2, "injected interruption after first rename");
                Ok(())
            },
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("interruption after first rename"));
    assert_eq!(root.read(first.path()).unwrap().content(), first.content());
    assert_eq!(
        root.read(second.path()).unwrap().content(),
        second.content()
    );
    assert!(polls >= 4, "monitor must continue through rollback");
}
