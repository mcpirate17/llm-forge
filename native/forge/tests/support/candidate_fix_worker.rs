//! Native stdin fixture, compiled only by explicitly ignored execution tests.
use std::{
    env, fs,
    io::{self, Read, Write},
    path::PathBuf,
    process, thread,
    time::{Duration, Instant},
};

fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--version"] {
        println!("ruff 0.16.5");
        return;
    }
    let stage = args.first().expect("check/format stage");
    assert!(matches!(stage.as_str(), "check" | "format"));
    assert!(args.windows(2).any(|pair| pair == ["--color", "never"]));
    assert!(args.iter().any(|arg| arg == "--no-cache"));
    assert_eq!(args[args.len() - 2..], ["--", "-"]);
    let selected = args
        .windows(2)
        .find(|pair| pair[0] == "--stdin-filename")
        .map(|pair| PathBuf::from(&pair[1]))
        .expect("explicit source filename");
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input).unwrap();
    let mode = env::var("FORGE_FIXTURE_MODE").unwrap_or_else(|_| "success".into());
    if stage == "check" && matches!(mode.as_str(), "pause" | "timeout") {
        let home = PathBuf::from(env::var_os("HOME").unwrap());
        fs::write(home.join("ready"), process::id().to_string()).unwrap();
        let end = Instant::now() + Duration::from_secs(30);
        while !home.join("release").exists() && Instant::now() < end {
            thread::sleep(Duration::from_millis(10));
        }
    }
    if stage == "check" && mode == "signal" {
        // A real fatal child signal, independent of the parent's signalfd mask.
        process::abort();
    }
    if (stage == "check" && mode == "partial-fail")
        || (stage == "format" && mode == "format-fail")
        || (stage == "check" && mode == "second-fail" && selected.ends_with("second.py"))
    {
        io::stdout().write_all(b"partial candidate\r\n").unwrap();
        eprintln!("fixture refusal after partial stdout");
        process::exit(7);
    }
    if stage == "check" && mode == "outside" {
        let outside = env::var_os("FORGE_FIXTURE_OUTSIDE").unwrap();
        assert_eq!(
            fs::write(outside, b"unauthorized").unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        // Source is a read capability too, including the selected input itself.
        assert_eq!(
            fs::write(&selected, b"unauthorized").unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        eprintln!("out-of-scope writes denied");
    }
    let text = String::from_utf8(input).unwrap();
    let output = if mode == "parrot" {
        text
    } else if stage == "check" {
        text.replace("import unused\r\n", "")
            .replace("import unused\n", "")
    } else {
        text.replace("x=1", "x = 1").replace("y=2", "y = 2")
    };
    eprintln!("fixture diagnostic: {stage}");
    io::stdout().write_all(output.as_bytes()).unwrap();
}
