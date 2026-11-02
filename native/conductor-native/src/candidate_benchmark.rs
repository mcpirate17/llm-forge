//! Prospectively versioned benchmark source data, without an interpreter.
//! Frozen optional guest inputs are not the benchmark coordinator.

pub const WORKLOAD_REVISION: &str = "native-review-fixtures-v1";
pub const PYTHON_INPUT_SUFFIX: &str = "candidate_review/benchmark_probe.py";
pub const PYTHON_BASE: &[u8] = include_bytes!("candidate_benchmark/python_input.py");
const MARKER: &[u8] = b"Frozen Python analysis workload; never a review coordinator.";
const REPLACEMENT: &[u8] =
    b"Frozen Python analysis workload; never a review coordinator. Python benchmark probe.";
pub const RUST_INPUT_PATH: &str = "native/benchmark_probe/src/lib.rs";
pub const RUST_BASE: &[u8] = b"pub fn identity(value: u64) -> u64 { value }\n";
pub const RUST_CHANGED: &[u8] = b"pub fn identity(value: u64) -> u64 { value.wrapping_add(0) }\n";
pub const RUST_MANIFEST: &[u8] =
    b"[package]\nname='candidate_benchmark_probe'\nversion='0.0.0'\nedition='2021'\n[workspace]\n";

/// Refuse changed fixture markers rather than silently choosing another edit.
pub fn python_projection(content: &[u8]) -> Result<Vec<u8>, String> {
    if content.len() > 1024 * 1024 {
        return Err("Python fixture exceeds 1 MiB".into());
    }
    let mut offsets = content
        .windows(MARKER.len())
        .enumerate()
        .filter_map(|(offset, value)| (value == MARKER).then_some(offset));
    let offset = offsets.next().ok_or("Python fixture marker is missing")?;
    if offsets.next().is_some() {
        return Err("Python fixture marker is ambiguous".into());
    }
    let mut changed = Vec::with_capacity(content.len() + REPLACEMENT.len());
    changed.extend_from_slice(&content[..offset]);
    changed.extend_from_slice(REPLACEMENT);
    changed.extend_from_slice(&content[offset + MARKER.len()..]);
    Ok(changed)
}

pub fn python_path(package_root: &str) -> Result<String, String> {
    if package_root.len() > 4000 || package_root.bytes().any(|b| b.is_ascii_control()) {
        return Err(
            "benchmark package_root exceeds 4000 bytes or contains control characters".into(),
        );
    }
    let package = crate::project_paths::relative(package_root, "benchmark package_root")?;
    if package.split('/').any(|p| p.eq_ignore_ascii_case(".git")) {
        return Err("benchmark package_root cannot address Git metadata".into());
    }
    Ok(format!("{package}/{PYTHON_INPUT_SUFFIX}"))
}
pub fn python_input(package_root: &str) -> Result<(String, Vec<u8>), String> {
    Ok((python_path(package_root)?, python_projection(PYTHON_BASE)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_python_edit_changes_only_one_docstring_line() {
        let (path, changed) = python_input("conductor").unwrap();
        assert_eq!(path, "conductor/candidate_review/benchmark_probe.py");
        let original = std::str::from_utf8(PYTHON_BASE).unwrap();
        let changed = std::str::from_utf8(&changed).unwrap();
        assert_eq!(original.lines().count(), changed.lines().count());
        assert_eq!(
            original.lines().skip(1).collect::<Vec<_>>(),
            changed.lines().skip(1).collect::<Vec<_>>()
        );
        assert_ne!(original.lines().next(), changed.lines().next());
    }
    #[test]
    fn source_drift_refuses_missing_or_ambiguous_marker() {
        assert!(python_projection(b"unrelated")
            .unwrap_err()
            .contains("missing"));
        let twice = [PYTHON_BASE, PYTHON_BASE].concat();
        assert!(python_projection(&twice).unwrap_err().contains("ambiguous"));
    }
    #[test]
    fn bounded_fixture_inputs_refuse_git_metadata_and_oversized_payloads() {
        assert!(python_path(".git").is_err());
        assert!(python_path("../outside").is_err());
        assert!(python_path(&"x".repeat(4001)).is_err());
        assert!(python_projection(&vec![b'x'; 1024 * 1024 + 1]).is_err());
    }
}
