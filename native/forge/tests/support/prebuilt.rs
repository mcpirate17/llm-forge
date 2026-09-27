//! Reuse developer-install fixtures only while their inputs and bytes match.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

fn sha256(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut chunk).ok()?;
        if count == 0 {
            break;
        }
        hasher.update(&chunk[..count]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

pub fn verified_prebuilt(
    binary_relative: &str,
    proof_name: &str,
    sources: &[PathBuf],
) -> Option<PathBuf> {
    let profile_dir = Path::new(env!("CARGO_BIN_EXE_forge")).parent()?;
    let fixture_dir = profile_dir.join("prebuilt-fixtures");
    let binary = fixture_dir.join(binary_relative);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(&binary).ok()?.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    let proof = std::fs::read_to_string(fixture_dir.join(proof_name)).ok()?;
    let expected: Vec<_> = proof.lines().collect();
    if expected.len() != sources.len() + 1 {
        return None;
    }
    for (path, expected_hash) in sources.iter().zip(expected.iter()) {
        if sha256(path).as_deref() != Some(*expected_hash) {
            return None;
        }
    }
    (sha256(&binary).as_deref() == expected.last().copied()).then_some(binary)
}
