//! Native external-probe fixture; never mutates refs or worktrees.
use std::{
    fs,
    io::{self, Write},
    path::Path,
};
fn main() -> io::Result<()> {
    let name = std::env::args_os().next().unwrap();
    let name = Path::new(&name).file_name().unwrap().to_str().unwrap();
    if name == "gh" {
        let mode = std::env::var("FORGE_BRANCH_GH_MODE").unwrap_or("empty".into());
        if mode == "fail" {
            eprintln!("fixture PR service unavailable");
            std::process::exit(7);
        }
        if mode == "merged" {
            println!("[{{\"state\":\"MERGED\"}}]");
        } else {
            println!("[]");
        }
        return Ok(());
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.starts_with(&["worktree".into(), "list".into()]) {
        let bytes = fs::read(std::env::var_os("FORGE_BRANCH_WORKTREE_INVENTORY").unwrap())?;
        io::stdout().write_all(&bytes)?;
        return Ok(());
    }
    if args.first().is_some_and(|value| value == "ls-remote") {
        return Ok(());
    }
    Err(io::Error::other("unexpected native fixture Git operation"))
}
