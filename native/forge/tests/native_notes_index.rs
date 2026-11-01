//! Native notes-index CLI contracts over isolated temporary files only.
#[allow(dead_code)]
mod support;
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, UNIX_EPOCH};

struct Fixture {
    host: support::Host,
    hostdir: PathBuf,
    vault: PathBuf,
    db: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let host = support::Host::new();
        let hostdir = host.0.join("host");
        fs::create_dir_all(&hostdir).unwrap();
        let vault = host.0.join("vault");
        let db = host.0.join("notes.db");
        let f = Self {
            host,
            hostdir,
            vault,
            db,
        };
        for i in 1..=8 {
            f.write(
                &format!("research/notes/note{i}.md"),
                &format!("# Note {i}\n\nbody text {i} apple\n"),
            );
        }
        f.write(
            "research/notes/note_titled.md",
            "# Titled Heading\n\nprose about apple banana\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        );
        f.write("research/notes/note_untitled.md", "prose with no heading, mentions apple\n\n| x | y | z |\n|---|---|---|\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |\n");
        f.write("tasks/task1.md", "# Task One\n\ntask body apple\n");
        f.write("tasks/task2.md", "# Task Two\n\ntask body banana\n");
        f.write(
            "tasks/audit/excluded.md",
            "# Excluded\n\nshould never be indexed apple\n",
        );
        for i in 1..=4 {
            f.write(
                &format!("vault/research/vnote{i}.md"),
                &format!("# Vault Note {i}\n\nvault body apple {i}\n"),
            );
        }
        f.write(
            "vault/dashboards/dash1.md",
            "# Dashboard One\n\ndashboard body apple\n",
        );
        f.write(
            "vault/runbooks/runbook1.md",
            "# Runbook One\n\nrunbook body apple\n",
        );
        Connection::open(&f.db)
            .unwrap()
            .execute_batch(
                "CREATE TABLE unrelated (value TEXT); INSERT INTO unrelated VALUES ('sentinel');",
            )
            .unwrap();
        f
    }
    fn write(&self, rel: &str, text: &str) {
        let path = if let Some(rel) = rel.strip_prefix("vault/") {
            self.vault.join(rel)
        } else {
            self.hostdir.join(rel)
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn command(&self, action: &str) -> Command {
        let mut cmd = self.host.command();
        cmd.env("PATH", "")
            .env_remove("CONDUCTOR_NOTES_ROOT")
            .env_remove("CONDUCTOR_NOTES_DB")
            .args(["notes", action, "--host"])
            .arg(&self.hostdir)
            .arg("--db")
            .arg(&self.db);
        cmd
    }
    fn index(&self) -> Output {
        let mut cmd = self.command("index");
        cmd.arg("--vault").arg(&self.vault);
        run(&mut cmd)
    }
    fn indexed(&self) {
        let out = self.index();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fn search(&self, query: &str) -> Vec<String> {
        let mut cmd = self.command("search");
        cmd.args(["--json", query]);
        let out = run(&mut cmd);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Vec<Value>>(&out.stdout)
            .unwrap()
            .iter()
            .map(|v| v["path"].as_str().unwrap().into())
            .collect()
    }
}
fn run(command: &mut Command) -> Output {
    forge::process_probe::run(command, Duration::from_secs(10), 1024 * 1024)
        .expect("bounded notes command")
}
type NoteRow = (String, String, String, String, f64);
type TableRow = (
    String,
    String,
    String,
    i64,
    String,
    i64,
    i64,
    String,
    String,
);
fn notes(db: &Path) -> Vec<NoteRow> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare("SELECT path,source,title,body,mtime FROM notes_fts ORDER BY path,source")
        .unwrap();
    stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}
fn tables(db: &Path) -> Vec<TableRow> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn.prepare("SELECT source,path,note,table_idx,section_heading,n_cols,n_rows,headers_json,rows_json FROM note_tables ORDER BY note,table_idx").unwrap();
    stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
            r.get(7)?,
            r.get(8)?,
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}
fn source_file(f: &Fixture, row: &NoteRow) -> PathBuf {
    match row.1.as_str() {
        "notes" | "tasks" => f.hostdir.join(&row.0),
        "vault_research" | "vault_dashboards" | "vault_runbooks" => f.vault.join(&row.0),
        other => panic!("unexpected source {other}"),
    }
}
#[test]
fn indexes_full_fixture_and_preserves_tables_and_mtime() {
    let f = Fixture::new();
    f.indexed();
    let rows = notes(&f.db);
    assert_eq!(rows.len(), 18);
    for row in &rows {
        let path = source_file(&f, row);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(row.3, text);
        assert!(!row.0.starts_with("tasks/audit/"));
        let title = text
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .unwrap_or_else(|| path.file_name().unwrap().to_str().unwrap());
        assert_eq!(row.2, title);
        let mtime = fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert!((row.4 - mtime).abs() < 1e-6);
    }
    assert_eq!(
        tables(&f.db),
        vec![
            (
                "notes".into(),
                "research/notes/note_titled.md".into(),
                "note_titled.md".into(),
                0,
                "Titled Heading".into(),
                2,
                1,
                "[\"a\", \"b\"]".into(),
                "[[\"1\", \"2\"]]".into()
            ),
            (
                "notes".into(),
                "research/notes/note_untitled.md".into(),
                "note_untitled.md".into(),
                0,
                "".into(),
                3,
                2,
                "[\"x\", \"y\", \"z\"]".into(),
                "[[\"1\", \"2\", \"3\"], [\"4\", \"5\", \"6\"]]".into()
            ),
        ]
    );
    let value: String = Connection::open(&f.db)
        .unwrap()
        .query_row("SELECT value FROM unrelated", [], |r| r.get(0))
        .unwrap();
    assert_eq!(value, "sentinel");
}
#[test]
fn search_rank_order_and_deletion_reindex_are_stable() {
    let f = Fixture::new();
    f.indexed();
    let mut apple = vec![
        "tasks/task1.md".to_string(),
        "dashboards/dash1.md".into(),
        "runbooks/runbook1.md".into(),
    ];
    apple.extend((1..=8).map(|i| format!("research/notes/note{i}.md")));
    apple.extend((1..=4).map(|i| format!("research/vnote{i}.md")));
    apple.extend([
        "research/notes/note_titled.md".into(),
        "research/notes/note_untitled.md".into(),
    ]);
    assert_eq!(f.search("apple"), apple);
    assert_eq!(
        f.search("banana"),
        ["tasks/task2.md", "research/notes/note_titled.md"]
    );
    assert_eq!(
        f.search("Titled Heading"),
        ["research/notes/note_titled.md"]
    );
    let old = notes(&f.db);
    f.indexed();
    assert_eq!(notes(&f.db), old);
    fs::remove_file(f.hostdir.join("research/notes/note1.md")).unwrap();
    f.indexed();
    assert_eq!(notes(&f.db).len(), 17);
    assert!(!f
        .search("apple")
        .contains(&"research/notes/note1.md".to_string()));
}
#[test]
fn tables_cli_filters_and_has_bounded_human_and_json_output() {
    let f = Fixture::new();
    f.indexed();
    let mut cmd = f.command("tables");
    cmd.arg("--json");
    let out = run(&mut cmd);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["note"], "note_titled.md");
    assert_eq!(rows[0]["table_idx"], 0);
    assert_eq!(rows[0]["section_heading"], "Titled Heading");
    assert_eq!(rows[0]["n_cols"], 2);
    assert_eq!(rows[0]["n_rows"], 1);
    assert_eq!(rows[0]["headers"], serde_json::json!(["a", "b"]));
    let mut cmd = f.command("tables");
    cmd.args(["--json", "untitled"]);
    let out = run(&mut cmd);
    assert!(out.status.success());
    let filtered: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0]["note"], "note_untitled.md");
    let mut cmd = f.command("tables");
    cmd.args(["--limit", "1"]);
    let out = run(&mut cmd);
    assert!(out.status.success());
    let human = String::from_utf8(out.stdout).unwrap();
    assert!(human.contains("note_titled.md") && human.contains("cols:"));
    assert!(!human.contains("note_untitled.md"));
    for limit in ["0", "1001"] {
        let mut cmd = f.command("tables");
        cmd.args(["--limit", limit]);
        assert!(!run(&mut cmd).status.success());
    }
    let mut cmd = f.command("tables");
    cmd.arg("a".repeat(4097));
    assert!(!run(&mut cmd).status.success());
}
#[test]
fn missing_previous_root_and_invalid_utf8_preserve_existing_index() {
    let f = Fixture::new();
    f.indexed();
    let before = notes(&f.db);
    let before_tables = tables(&f.db);
    fs::rename(f.hostdir.join("tasks"), f.hostdir.join("tasks-away")).unwrap();
    assert!(!f.index().status.success());
    assert_eq!(notes(&f.db), before);
    assert_eq!(tables(&f.db), before_tables);
    fs::rename(f.hostdir.join("tasks-away"), f.hostdir.join("tasks")).unwrap();
    fs::write(f.hostdir.join("research/notes/invalid.md"), [0xff, 0xfe]).unwrap();
    assert!(!f.index().status.success());
    assert_eq!(notes(&f.db), before);
    assert_eq!(tables(&f.db), before_tables);
}
#[test]
fn configured_note_and_database_paths_are_honored() {
    let f = Fixture::new();
    fs::rename(f.hostdir.join("research/notes"), f.hostdir.join("journal")).unwrap();
    f.write(
        "forge.toml",
        "schema_version=1\n[conductor]\nnotes_root='journal'\nnotes_db='custom.db'\n",
    );
    let db = f.hostdir.join("custom.db");
    Connection::open(&db).unwrap();
    let mut index = f.host.command();
    index
        .env("PATH", "")
        .env_remove("CONDUCTOR_NOTES_ROOT")
        .env_remove("CONDUCTOR_NOTES_DB")
        .args(["notes", "index", "--host"])
        .arg(&f.hostdir)
        .arg("--vault")
        .arg(&f.vault);
    let out = run(&mut index);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows = notes(&db);
    assert_eq!(rows.len(), 18);
    assert!(rows
        .iter()
        .any(|r| r.0 == "journal/note1.md" && r.1 == "notes"));
    assert!(!rows.iter().any(|r| r.0.starts_with("research/notes/")));
}
#[test]
fn sqlite_contention_is_bounded_then_rebuild_can_retry() {
    let f = Fixture::new();
    f.indexed();
    let before = notes(&f.db);
    let lock = Connection::open(&f.db).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let start = Instant::now();
    let out = f.index();
    assert!(!out.status.success());
    assert!(start.elapsed() < Duration::from_secs(5));
    lock.execute_batch("ROLLBACK").unwrap();
    assert_eq!(notes(&f.db), before);
    f.indexed();
    assert_eq!(notes(&f.db), before);
}
fn assert_unsafe_db(f: &Fixture, path: &Path) {
    let mut cmd = f.host.command();
    cmd.env("PATH", "")
        .args(["notes", "index", "--host"])
        .arg(&f.hostdir)
        .arg("--vault")
        .arg(&f.vault)
        .arg("--db")
        .arg(path);
    assert!(
        !run(&mut cmd).status.success(),
        "unsafe DB was accepted: {}",
        path.display()
    );
}
#[test]
fn unsafe_database_nodes_and_ancestors_are_refused() {
    let f = Fixture::new();
    f.indexed();
    let before = fs::read(&f.db).unwrap();
    let link = f.host.0.join("db-link");
    symlink(&f.db, &link).unwrap();
    assert_unsafe_db(&f, &link);
    for action in ["search", "tables"] {
        let mut cmd = f.host.command();
        cmd.env("PATH", "")
            .args(["notes", action, "--host"])
            .arg(&f.hostdir)
            .arg("--db")
            .arg(&link)
            .arg("--json");
        if action == "search" {
            cmd.arg("apple");
        }
        assert!(!run(&mut cmd).status.success());
    }
    let hard = f.host.0.join("db-hard");
    fs::hard_link(&f.db, &hard).unwrap();
    assert_unsafe_db(&f, &hard);
    fs::remove_file(&hard).unwrap();
    assert_eq!(fs::read(&f.db).unwrap(), before);
    let fifo = f.host.0.join("db-fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert_unsafe_db(&f, &fifo);
    let dirlink = f.host.0.join("ancestor-link");
    symlink(&f.host.0, &dirlink).unwrap();
    assert_unsafe_db(&f, &dirlink.join("notes.db"));
    assert_eq!(fs::read(&f.db).unwrap(), before);
}
#[test]
fn table_json_preserves_python_unicode_escaping() {
    let f = Fixture::new();
    f.write(
        "research/notes/unicode.md",
        "# Unicode\n\n| café | 🦀 |\n|---|---|\n| é | 中 |\n",
    );
    f.indexed();
    let table = tables(&f.db)
        .into_iter()
        .find(|row| row.2 == "unicode.md")
        .unwrap();
    assert_eq!(table.7, "[\"caf\\u00e9\", \"\\ud83e\\udd80\"]");
    assert_eq!(table.8, "[[\"\\u00e9\", \"\\u4e2d\"]]");
}
fn sidecar_path(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
#[test]
fn unsafe_sqlite_sidecars_are_refused_without_data_changes() {
    let f = Fixture::new();
    f.indexed();
    let before = fs::read(&f.db).unwrap();
    let expected = notes(&f.db);
    let sentinel = f.host.0.join("sentinel");
    for suffix in ["-journal", "-wal", "-shm"] {
        for kind in ["symlink", "hardlink", "fifo"] {
            fs::write(&sentinel, b"unchanged").unwrap();
            let sidecar = sidecar_path(&f.db, suffix);
            match kind {
                "symlink" => symlink(&sentinel, &sidecar).unwrap(),
                "hardlink" => fs::hard_link(&sentinel, &sidecar).unwrap(),
                _ => {
                    let name =
                        std::ffi::CString::new(sidecar.as_os_str().as_encoded_bytes()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                }
            }
            let out = f.index();
            assert!(!out.status.success(), "accepted {kind} sidecar {suffix}");
            assert_eq!(fs::read(&sentinel).unwrap(), b"unchanged");
            assert_eq!(fs::read(&f.db).unwrap(), before);
            fs::remove_file(sidecar).unwrap();
            assert_eq!(notes(&f.db), expected);
        }
    }
}
#[test]
fn custom_index_trigger_is_refused_without_changing_other_tables() {
    let f = Fixture::new();
    f.indexed();
    let conn = Connection::open(&f.db).unwrap();
    conn.execute_batch("CREATE TRIGGER bad_trigger AFTER DELETE ON note_tables BEGIN UPDATE unrelated SET value='changed'; END;").unwrap();
    let before = notes(&f.db);
    let old_tables = tables(&f.db);
    let out = f.index();
    assert!(!out.status.success());
    let value: String = conn
        .query_row("SELECT value FROM unrelated", [], |r| r.get(0))
        .unwrap();
    assert_eq!(value, "sentinel");
    assert_eq!(notes(&f.db), before);
    assert_eq!(tables(&f.db), old_tables);
}
#[test]
fn wal_mode_rebuild_and_read_preserve_unrelated_data() {
    let f = Fixture::new();
    let keeper = Connection::open(&f.db).unwrap();
    let mode: String = keeper
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    f.indexed();
    assert_eq!(notes(&f.db).len(), 18);
    let value: String = keeper
        .query_row("SELECT value FROM unrelated", [], |r| r.get(0))
        .unwrap();
    assert_eq!(value, "sentinel");
    assert_eq!(
        f.search("Titled Heading"),
        ["research/notes/note_titled.md"]
    );
    f.indexed();
    assert_eq!(tables(&f.db).len(), 2);
    let value: String = keeper
        .query_row("SELECT value FROM unrelated", [], |r| r.get(0))
        .unwrap();
    assert_eq!(value, "sentinel");
}
#[test]
fn ordinary_notes_fts_impostor_is_refused_and_existing_rows_remain() {
    let f = Fixture::new();
    let conn = Connection::open(&f.db).unwrap();
    conn.execute_batch("CREATE TABLE notes_fts(path TEXT, source TEXT, title TEXT, body TEXT, mtime REAL); INSERT INTO notes_fts VALUES ('old/path','notes','old title','old body',123.0);").unwrap();
    let out = f.index();
    assert!(!out.status.success(), "ordinary table was accepted as FTS5");
    let row: (String, String, String, String, f64) = conn
        .query_row(
            "SELECT path,source,title,body,mtime FROM notes_fts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "old/path".into(),
            "notes".into(),
            "old title".into(),
            "old body".into(),
            123.0
        )
    );
    let value: String = conn
        .query_row("SELECT value FROM unrelated", [], |r| r.get(0))
        .unwrap();
    assert_eq!(value, "sentinel");
}
#[test]
fn incompatible_fts_options_are_refused_without_replacing_index() {
    for schema in [
        "CREATE VIRTUAL TABLE notes_fts USING fts5(path, source UNINDEXED, title, body, mtime UNINDEXED)",
        "CREATE VIRTUAL TABLE notes_fts USING fts5(path UNINDEXED, source UNINDEXED, title, body, mtime UNINDEXED, content='')",
    ] {
        let f=Fixture::new(); let conn=Connection::open(&f.db).unwrap();
        conn.execute_batch(schema).unwrap();
        conn.execute("INSERT INTO notes_fts(path,source,title,body,mtime) VALUES ('old/path','notes','old title','old body',123.0)",[]).unwrap();
        let original: i64=conn.query_row("SELECT count(*) FROM notes_fts",[],|r|r.get(0)).unwrap();
        assert!(!f.index().status.success(),"incompatible FTS5 options accepted: {schema}");
        let after: i64=conn.query_row("SELECT count(*) FROM notes_fts",[],|r|r.get(0)).unwrap(); assert_eq!(after,original);
        let value: String=conn.query_row("SELECT value FROM unrelated",[],|r|r.get(0)).unwrap(); assert_eq!(value,"sentinel");
    }
}
#[test]
fn concurrent_rebuilds_and_readers_observe_complete_snapshots() {
    let f = Fixture::new();
    f.indexed();
    let expected_notes = notes(&f.db);
    let expected_tables = tables(&f.db);
    let expected_search = f.search("apple");
    let successes = std::thread::scope(|scope| {
        let mut writers = Vec::new();
        for _ in 0..4 {
            let fixture = &f;
            writers.push(scope.spawn(move || {
                let mut success = 0usize;
                for _ in 0..4 {
                    let output = fixture.index();
                    if output.status.success() {
                        success += 1;
                    } else {
                        let error = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
                        assert!(
                            error.contains("busy") || error.contains("locked"),
                            "unexpected writer error: {error}"
                        );
                    }
                }
                success
            }));
        }
        let fixture = &f;
        let expected = &expected_search;
        let reader = scope.spawn(move || {
            for _ in 0..8 {
                assert_eq!(
                    &fixture.search("apple"),
                    expected,
                    "reader observed a partial snapshot"
                );
            }
        });
        let total: usize = writers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .sum();
        reader.join().unwrap();
        total
    });
    assert!(successes > 0, "all concurrent rebuilds failed");
    f.indexed();
    assert_eq!(notes(&f.db), expected_notes);
    assert_eq!(tables(&f.db), expected_tables);
    let sentinel: String = Connection::open(&f.db)
        .unwrap()
        .query_row("SELECT value FROM unrelated", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sentinel, "sentinel");
}
