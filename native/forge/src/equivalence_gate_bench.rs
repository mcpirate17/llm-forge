//! Opt-in preparation-only cost observation; never launches a guest.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    time::Instant,
};

fn elapsed(start: Instant) -> u128 {
    start.elapsed().as_nanos()
}
fn put(root: &std::path::Path, path: &str, bytes: &[u8]) {
    let dest = root.join(path);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(dest, bytes).unwrap();
}
fn fixture(extra: usize) -> Fixture {
    let mut fixture = Fixture::new();
    let root = fixture.source.root();
    put(root, "src/probe.py", b"def value(x):\n    return x + 1\n");
    put(
        root,
        "tests/test_probe.py",
        b"from src.probe import value\ndef test_value():\n    assert value(1) == 2\n",
    );
    put(root, "data/base.bin", &[7; 1024]);
    for n in 0..extra {
        put(root, &format!("data/item-{n:04}.bin"), &[n as u8; 1024]);
    }
    let repository = fixture.source.repository();
    let mut index = repository.index().unwrap();
    index
        .add_all(
            ["src", "tests", "data"],
            git2::IndexAddOption::DEFAULT,
            None,
        )
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repository.find_tree(tree_id).unwrap();
    let previous = repository.head().unwrap().peel_to_commit().unwrap();
    let signature = Signature::now("fixture", "fixture@example.invalid").unwrap();
    let commit = repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "benchmark fixture",
            &tree,
            &[&previous],
        )
        .unwrap()
        .to_string();
    drop(tree);
    drop(previous);
    drop(index);
    fixture.commit = commit;
    fixture
}
fn observation(f: &Fixture, live: bool) -> anyhow::Result<Value> {
    let total = Instant::now();
    let start = Instant::now();
    let mut requested = args();
    requested.module = "src/probe.py".into();
    requested.commit = if live { None } else { Some(f.commit.clone()) };
    requested.live = live;
    requested.inputs = if live {
        vec!["src".into(), "tests".into(), "data".into()]
    } else {
        vec![]
    };
    let origin = Source::open(f.source.root())?;
    let selected = SelectedSource::capture(&requested, origin, &f.grants)?;
    let snapshot = Snapshot::create(
        selected.tree_source(),
        selected.tree(),
        &f.grants
            .scratch_parent
            .join(crate::resource_id("equivalence-observe")?),
        SnapshotLimits::default(),
    )?;
    let capture_ns = elapsed(start);
    let start = Instant::now();
    let captured = candidate_equivalence_inputs::capture_committed(
        CaptureContext {
            origin: selected.origin(),
            tree_source: selected.tree_source(),
            snapshot: &snapshot,
            grants: &f.grants,
        },
        &requested.module,
        None,
        None,
        1,
    )?;
    let targets = serde_json::to_value(&captured.targets)?;
    let evidence = captured.evidence.clone();
    let workspace = selected.workspace(&f.grants)?;
    let prepare_ns = elapsed(start);
    let start = Instant::now();
    selected.verify()?;
    snapshot.verify_materialized(selected.tree_source())?;
    captured.verify()?;
    workspace.verify_materialized(selected.tree_source())?;
    let verify_ns = elapsed(start);
    let tree = selected.tree().to_owned();
    let live_inputs = selected.live_evidence();
    let start = Instant::now();
    drop(captured);
    workspace.discard()?;
    snapshot.discard()?;
    selected.discard()?;
    let remaining = fs::read_dir(&f.grants.scratch_parent)?.count();
    anyhow::ensure!(remaining == 0, "scratch not empty after disposal");
    let disposal_ns = elapsed(start);
    Ok(
        json!({"tree_oid":tree,"targets":targets,"capture_evidence":evidence,
        "live_inputs":live_inputs,"scratch_remaining":remaining,
        "capture_ns":capture_ns,"prepare_ns":prepare_ns,"verify_ns":verify_ns,
        "disposal_ns":disposal_ns,"total_ns":elapsed(total)}),
    )
}
fn observer_sha256() -> anyhow::Result<String> {
    let mut file = fs::File::open(std::env::current_exe()?)?;
    let mut hasher = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn write_report(path: &std::path::Path, report: &Value) -> anyhow::Result<()> {
    anyhow::ensure!(path.is_absolute(), "benchmark output must be absolute");
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer(&mut file, report)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
#[test]
#[ignore = "preparation-only cost observation; set FORGE_TEST_GATE_BENCH_OUTPUT"]
fn compare_committed_and_live_preparation() {
    let path = PathBuf::from(
        std::env::var_os("FORGE_TEST_GATE_BENCH_OUTPUT").expect("explicit output path required"),
    );
    let observer = observer_sha256().unwrap();
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for extra in [0, 1000] {
        let fixture = fixture(extra);
        let mut reference: Option<(Value, Value)> = None;
        for iteration in 0..12 {
            for live in if iteration % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let result = observation(&fixture, live);
                let mut row = json!({"extra_files":extra,"iteration":iteration,
                    "phase":if iteration < 2 {"warmup"} else {"scored"},
                    "arm":if live {"live"} else {"commit"}});
                match result {
                    Ok(value) => {
                        let identity = (value["tree_oid"].clone(), value["targets"].clone());
                        if let Some(expected) = &reference {
                            if *expected != identity {
                                errors.push(format!(
                                    "tree or target mismatch: {extra}/{iteration}/{live}"
                                ));
                            }
                        } else {
                            reference = Some(identity);
                        }
                        row["status"] = json!("completed");
                        row["observation"] = value;
                    }
                    Err(error) => {
                        row["status"] = json!("failed");
                        row["error"] = json!(format!("{error:#}"));
                        errors.push(format!("sample failed: {extra}/{iteration}/{live}"));
                    }
                }
                rows.push(row);
            }
        }
    }
    let report = json!({"schema_version":1,"scope":"preparation only; no guest, Stage execution or performance claim",
        "observer_binary_sha256":observer,"warmups_per_arm":2,"samples_per_arm":10,
        "rows":rows,"errors":errors});
    write_report(&path, &report).unwrap();
    assert!(
        errors.is_empty(),
        "observation failed; raw rows retained at {}",
        path.display()
    );
}
