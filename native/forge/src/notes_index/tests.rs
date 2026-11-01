use super::*;

#[test]
fn read_only_index_enforces_vm_deadline_and_row_size() {
    let dir = tempfile_dir();
    let path = dir.join("notes.db");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch("CREATE TABLE data(value TEXT);")
        .unwrap();
    drop(writer);
    let reader = open_search(&path).unwrap().unwrap();
    assert!(reader
        .execute("INSERT INTO data VALUES ('forbidden')", [])
        .is_err());
    assert!(reader
        .query_row("SELECT printf('%.*c', 5000000, 'x')", [], |row| row
            .get::<_, String>(0))
        .is_err());
    let start = std::time::Instant::now();
    let error = reader
        .query_row(
            "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n) SELECT sum(x) FROM n",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_err();
    assert!(error.to_string().contains("interrupted"), "{error}");
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    drop(reader);
    std::fs::remove_dir_all(dir).unwrap();
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn fts5_is_available() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(body)")
        .expect("rusqlite must be built with an FTS5-enabled sqlite3");
}

#[test]
fn extract_tables_parses_header_sep_rows() {
    let text = "# Sec\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
    let tables = extract_tables(text);
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].section, "Sec");
    assert_eq!(tables[0].headers, vec!["a", "b"]);
    assert_eq!(tables[0].rows, vec![vec!["1".to_string(), "2".to_string()]]);
}

#[test]
fn rebuild_indexes_repo_and_vault_together() {
    let tmp = tempfile_dir();
    let host = tmp.join("repo");
    let vault = tmp.join("vault");
    write(
        &host.join("research/notes/alpha.md"),
        "# Alpha\n\ntfwd marker\n",
    );
    write(&host.join("tasks/beta.md"), "# Beta\n\nbeta body\n");
    write(&vault.join("research/gamma.md"), "# Gamma\n\nvault body\n");

    let mut conn = Connection::open_in_memory().unwrap();
    let (n_files, _n_tables) = rebuild(&mut conn, &host, Some(&vault)).unwrap();
    assert_eq!(n_files, 3);
    let mut stmt = conn
        .prepare("SELECT DISTINCT source FROM notes_fts ORDER BY source")
        .unwrap();
    let sources: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(sources, vec!["notes", "tasks", "vault_research"]);

    let hits = search_notes(&conn, "tfwd", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].path.ends_with("alpha.md"));
}

#[test]
fn default_db_is_the_notes_database_not_the_run_database() {
    // Parity pin with the Python reference's `DEFAULT_NOTES_DB`. Both sides
    // hardcoded `runs.db` until 2026-09-16, which sent the prose index to the
    // run database on any host that had split the two.
    let host = Path::new("/srv/host");
    assert_eq!(
        resolve_db_path(host, None).unwrap(),
        PathBuf::from("/srv/host/research/notes.db")
    );
}

#[test]
fn an_explicit_db_flag_wins_over_the_default() {
    let host = Path::new("/srv/host");
    let chosen = PathBuf::from("/tmp/other.db");
    assert_eq!(resolve_db_path(host, Some(&chosen)).unwrap(), chosen);
}

#[test]
fn uppercase_legacy_schema_is_accepted_and_its_triggers_are_refused() {
    let root = tempfile_dir();
    write(
        &root.join("research/notes/a.md"),
        "# Original\noriginal body\n",
    );
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&NOTES_FTS_DDL.to_ascii_uppercase())
        .unwrap();
    conn.execute_batch(&NOTE_TABLES_DDL.to_ascii_uppercase())
        .unwrap();
    rebuild(&mut conn, &root, None).unwrap();
    assert_eq!(search_notes(&conn, "original", 20).unwrap().len(), 1);
    conn.execute_batch(
        "CREATE TABLE unrelated(value TEXT); INSERT INTO unrelated VALUES ('keep');
        CREATE TRIGGER custom_index_trigger AFTER DELETE ON NOTE_TABLES
        BEGIN UPDATE unrelated SET value='changed'; END;",
    )
    .unwrap();
    assert!(rebuild(&mut conn, &root, None).is_err());
    assert_eq!(
        conn.query_row("SELECT value FROM unrelated", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "keep"
    );
    assert_eq!(search_notes(&conn, "original", 20).unwrap().len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_unicode_table_json_preserves_ascii_escapes_and_separators() {
    let headers = vec!["雪", "🦀", "e\u{301}", "\u{7f}", "\n"];
    assert_eq!(
        extract::to_python_json(&headers).unwrap(),
        r#"["\u96ea", "\ud83e\udd80", "e\u0301", "\u007f", "\n"]"#
    );
}

#[test]
fn read_and_resource_failures_preserve_previous_index_rows() {
    let root = tempfile_dir();
    write(
        &root.join("research/notes/a.md"),
        "# Original\noriginal searchable body\n",
    );
    let mut conn = Connection::open_in_memory().unwrap();
    rebuild(&mut conn, &root, None).unwrap();
    let second = root.join("research/notes/b.md");
    write(&second, "# Second\nmore bytes\n");
    let default = ingest::Limits::default();
    for limits in [
        ingest::Limits {
            files: 1,
            ..default
        },
        ingest::Limits {
            examined: 1,
            ..default
        },
        ingest::Limits {
            note_bytes: 1,
            ..default
        },
        ingest::Limits {
            total_bytes: 1,
            ..default
        },
    ] {
        assert!(ingest::rebuild(&mut conn, &root, None, limits, || Ok(())).is_err());
        assert_eq!(search_notes(&conn, "original", 20).unwrap().len(), 1);
        assert!(search_notes(&conn, "Second", 20).unwrap().is_empty());
    }
    for bytes in [vec![0xff], vec![b'x', 0, b'y']] {
        std::fs::write(&second, bytes).unwrap();
        assert!(rebuild(&mut conn, &root, None).is_err());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM notes_fts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_roots_and_symlinks_fail_without_erasing_previous_content() {
    use std::os::unix::fs::symlink;
    let root = tempfile_dir();
    let notes = root.join("research/notes");
    write(&notes.join("a.md"), "# Original\noriginal body\n");
    let mut conn = Connection::open_in_memory().unwrap();
    rebuild(&mut conn, &root, None).unwrap();
    std::fs::rename(&notes, root.join("saved")).unwrap();
    assert!(rebuild(&mut conn, &root, None).is_err());
    symlink(root.join("saved"), &notes).unwrap();
    assert!(rebuild(&mut conn, &root, None).is_err());
    std::fs::remove_file(&notes).unwrap();
    std::fs::rename(root.join("saved"), &notes).unwrap();
    symlink(notes.join("a.md"), notes.join("link.md")).unwrap();
    assert!(rebuild(&mut conn, &root, None).is_err());
    assert_eq!(search_notes(&conn, "original", 20).unwrap().len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ddl_and_rows_roll_back_when_final_publication_check_fails() {
    let root = tempfile_dir();
    write(
        &root.join("research/notes/a.md"),
        "# Original\noriginal body\n",
    );
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE unrelated(value TEXT); INSERT INTO unrelated VALUES ('keep');",
    )
    .unwrap();
    assert!(rebuild_checked(&mut conn, &root, None, || anyhow::bail!(
        "injected publication failure"
    ))
    .is_err());
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='notes_fts'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    rebuild(&mut conn, &root, None).unwrap();
    write(
        &root.join("research/notes/a.md"),
        "# Changed\nchanged body\n",
    );
    assert!(rebuild_checked(&mut conn, &root, None, || anyhow::bail!(
        "injected publication failure"
    ))
    .is_err());
    assert_eq!(search_notes(&conn, "original", 20).unwrap().len(), 1);
    assert!(search_notes(&conn, "changed", 20).unwrap().is_empty());
    assert_eq!(
        conn.query_row("SELECT value FROM unrelated", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "keep"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tables_preserve_filter_order_dimensions_headers_and_human_format() {
    let root = tempfile_dir();
    write(&root.join("research/notes/z.md"), "# Zed\n| z |\n|---|\n");
    write(
        &root.join("research/notes/a.md"),
        "# Results\n| **雪** | `score` |\n|---|---|\n| yes | 1 |\n",
    );
    let mut conn = Connection::open_in_memory().unwrap();
    rebuild(&mut conn, &root, None).unwrap();
    let hits = table_notes(&conn, ".md", 20).unwrap();
    assert_eq!(
        hits.iter().map(|h| h.note.as_str()).collect::<Vec<_>>(),
        vec!["a.md", "z.md"]
    );
    assert_eq!(
        tables::render(&hits[..1], false).unwrap(),
        "a.md [#0] (1x2) 'Results'\n    cols: 雪, score\n"
    );
    let json: serde_json::Value =
        serde_json::from_str(&tables::render(&hits[..1], true).unwrap()).unwrap();
    assert_eq!(
        json,
        serde_json::json!([{"note":"a.md","table_idx":0,"section_heading":"Results","n_cols":2,"n_rows":1,"headers":["雪","score"]}])
    );
    assert_eq!(table_notes(&conn, "a%", 20).unwrap().len(), 1);
    assert_eq!(table_notes(&conn, "", 1).unwrap().len(), 1);
    assert!(table_notes(&conn, "absent", 20).unwrap().is_empty());
    for limit in [0, 1001] {
        assert!(table_notes(&conn, "", limit).is_err());
    }
    assert!(table_notes(&conn, &"x".repeat(4097), 20).is_err());
    conn.execute(
        "UPDATE note_tables SET headers_json='[false]' WHERE note='a.md'",
        [],
    )
    .unwrap();
    assert!(table_notes(&conn, "a.md", 20).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tables_reject_oversized_output_instead_of_truncating_stored_headers() {
    let conn = Connection::open_in_memory().unwrap();
    ddl(&conn).unwrap();
    let headers = serde_json::to_string(&vec!["x".repeat(1024 * 1024)]).unwrap();
    conn.execute("INSERT INTO note_tables(source,path,note,table_idx,section_heading,n_cols,n_rows,headers_json,rows_json,ingested_at) VALUES ('notes','a.md','a.md',0,'',1,0,?1,'[]',0)",[headers]).unwrap();
    assert!(table_notes(&conn, "", 20).is_err());
}

#[test]
fn parent_components_require_real_traversable_prefixes() {
    use std::os::unix::fs::symlink;
    let root = tempfile_dir();
    write(&root.join("research/notes/a.md"), "# Valid\nvalid body\n");
    std::fs::create_dir(root.join("legitimate")).unwrap();
    symlink(root.join("legitimate"), root.join("link")).unwrap();
    std::fs::write(root.join("file"), "regular file").unwrap();
    let mut conn = Connection::open_in_memory().unwrap();
    assert_eq!(
        rebuild(&mut conn, &root.join("legitimate/.."), None).unwrap(),
        (1, 0)
    );
    for prefix in ["link", "missing", "file"] {
        assert!(
            rebuild(&mut conn, &root.join(prefix).join(".."), None).is_err(),
            "{prefix}"
        );
    }
    assert_eq!(search_notes(&conn, "valid", 20).unwrap().len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn empty_cli_search_still_enforces_query_and_limit_bounds() {
    let root = tempfile_dir();
    let db = root.join("notes.db");
    let mut writer = Connection::open(&db).unwrap();
    rebuild(&mut writer, &root, None).unwrap();
    drop(writer);
    let mut args = SearchArgs {
        host: root.clone(),
        query: vec![],
        limit: 20,
        db: Some(db),
        json: true,
    };
    for limit in [0, 1001] {
        args.limit = limit;
        assert!(run_search(&args)
            .unwrap_err()
            .to_string()
            .contains("limit must be in 1..1000"));
    }
    args.limit = 20;
    args.query = vec![" ".repeat(4097)];
    assert!(run_search(&args)
        .unwrap_err()
        .to_string()
        .contains("notes query exceeds 4096 bytes"));
    args.query.clear();
    assert_eq!(run_search(&args).unwrap(), 0);
    std::fs::remove_dir_all(root).unwrap();
}

fn tempfile_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "forge-notes-index-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
