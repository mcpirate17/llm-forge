//! Direct PyO3 call into the production snapshot API.

use crate::support::{module, path};
use pyo3::prelude::*;
use std::path::Path;

pub fn snapshot(py: Python<'_>, repo: &Path, owner: &str) -> Option<String> {
    module(py, "conductor.candidate_review.engine")
        .getattr("snapshot_working_tree")
        .unwrap()
        .call1((path(py, repo), owner))
        .unwrap()
        .extract()
        .unwrap()
}
