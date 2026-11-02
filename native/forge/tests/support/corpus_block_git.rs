//! Native stand-in for a blocked Git probe. Used only by opt-in CLI tests.
use std::{
    fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt, process::Command, time::Duration,
};
unsafe extern "C" {
    fn prctl(option: i32, ...) -> i32;
    fn getppid() -> i32;
}
fn guard_parent() {
    let parent = unsafe { getppid() };
    if let Ok(expected) = std::env::var("FORGE_TEST_EXPECT_PARENT") {
        assert_eq!(parent, expected.parse::<i32>().unwrap());
    }
    assert_eq!(unsafe { prctl(1, 9, 0, 0, 0) }, 0); // PR_SET_PDEATHSIG, SIGKILL
    assert_eq!(unsafe { getppid() }, parent);
}
fn main() {
    guard_parent();
    if std::env::var_os("FORGE_TEST_BLOCK_GIT_DESCENDANT").is_some() {
        let dir = std::path::PathBuf::from(std::env::var_os("FORGE_TEST_BLOCK_GIT_DIR").unwrap());
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join("ready"))
            .unwrap();
        std::thread::sleep(Duration::from_secs(60));
        return;
    }
    let child = Command::new(std::env::current_exe().unwrap())
        .env("FORGE_TEST_BLOCK_GIT_DESCENDANT", "1")
        .env("FORGE_TEST_EXPECT_PARENT", std::process::id().to_string())
        .spawn()
        .unwrap();
    let dir = std::path::PathBuf::from(std::env::var_os("FORGE_TEST_BLOCK_GIT_DIR").unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !dir.join("ready").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "descendant did not initialize"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let temporary = dir.join("pids.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .unwrap();
    writeln!(file, "{} {}", std::process::id(), child.id()).unwrap();
    file.sync_all().unwrap();
    std::fs::rename(temporary, dir.join("pids")).unwrap();
    std::thread::sleep(Duration::from_secs(60));
}
