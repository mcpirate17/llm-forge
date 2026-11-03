//! Minimal fixtures for retained changed-line source contracts.

use super::candidate_review_support as fixture;
use super::support::Case;
use std::path::{Path, PathBuf};

pub fn repo(case: &Case) -> PathBuf {
    let root = case.root().join("repo");
    fixture::init_repo(&root);
    root
}

pub fn write(root: &Path, relative: &str, body: &str) {
    fixture::write_fixture(root, relative, body);
}

pub fn numbered(prefix: &str, count: usize) -> String {
    (1..=count)
        .map(|n| format!("{prefix}_{n} = {n}\n"))
        .collect()
}
