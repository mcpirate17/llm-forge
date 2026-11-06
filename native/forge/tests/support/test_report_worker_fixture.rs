//! Standalone std-only SYNTHETIC worker. Compile separately with rustc; never a
//! Forge classifier or source-identity oracle. Reads authored control/JSON files.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
unsafe extern "C" {
    fn setsid() -> i32;
}

fn wait_until(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < end, "synthetic fixture gate expired");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn witness(home: &Path, name: &str, text: &str) {
    let next = home.join(format!("{name}-next"));
    fs::write(&next, text).unwrap();
    fs::rename(next, home.join(name)).unwrap();
}
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--detached") {
        assert_eq!(args.len(), 3);
        assert!(unsafe { setsid() } > 0);
        let root = Path::new(&args[1]);
        let home = Path::new(&args[2]);
        witness(home, "child-ready", &std::process::id().to_string());
        wait_until(|| fs::read_to_string(root.join("worker-mode")).unwrap() == "normal");
        return;
    }
    assert_eq!(args.len(), 8);
    assert_eq!((&*args[0], &*args[1]), ("harness", "test-report-worker"));
    assert_eq!(
        (&*args[2], &*args[4], &*args[6]),
        ("--root", "--config-root", "--max-bytes")
    );
    assert!((512..=262144).contains(&args[7].parse::<usize>().unwrap()));
    let root = Path::new(&args[3]);
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    assert!(std::env::var_os("FORGE_TEST_REPORT_SYNTHETIC_SECRET").is_none());
    let mode = fs::read_to_string(root.join("worker-mode")).unwrap();
    if mode == "nonzero" {
        eprintln!("synthetic controlled worker exit7 λ");
        std::process::exit(7);
    }
    if mode == "denied" {
        let outside = fs::read_to_string(root.join("worker-denied")).unwrap();
        assert_eq!(
            fs::read(outside).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
    let child = if mode == "descendant" {
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--detached")
            .arg(root)
            .arg(&home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_until(|| home.join("child-ready").is_file());
        Some(child)
    } else {
        None
    };
    witness(
        &home,
        "started.json",
        &format!(
            "{{\"pid\":{},\"child\":{},\"secret_present\":false}}",
            std::process::id(),
            child.as_ref().map_or(0, std::process::Child::id)
        ),
    );
    if matches!(mode.as_str(), "hang" | "descendant") {
        wait_until(|| fs::read_to_string(root.join("worker-mode")).unwrap() == "normal");
    }
    let output = match mode.as_str() {
        "invalid" => b"not JSON\n".to_vec(),
        "large" => fs::read(root.join("worker-large-response.json")).unwrap(),
        "normal" | "hang" | "descendant" | "denied" => {
            fs::read(root.join("worker-response.json")).unwrap()
        }
        _ => panic!("unknown synthetic worker mode"),
    };
    std::io::stdout().write_all(&output).unwrap();
    std::io::stdout().write_all(b"\n").unwrap();
    // The intentionally detached child is owned by the actual confined actor's
    // cgroup, not by this cooperative fixture's process handles.
    drop(child);
}
