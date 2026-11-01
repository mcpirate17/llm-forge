#![allow(dead_code)]
use conductor_native::vector_index::{normalize, Space};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Fixture(pub PathBuf);
impl Fixture {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-vector-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    pub fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
pub fn space(dimension: usize) -> Space {
    Space {
        dimension,
        model_sha256: [0x7d; 32],
    }
}
pub struct Random(u64);
impl Random {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut bits = self.0;
        bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d049bb133111eb);
        bits ^= bits >> 31;
        (bits >> 40) as f32 / (1 << 24) as f32 * 2.0 - 1.0
    }
}
pub fn dataset(
    rows: usize,
    dimension: usize,
    clusters: usize,
    seed: u64,
) -> (Vec<f32>, Vec<Vec<f32>>) {
    let mut rng = Random::new(seed);
    let centers: Vec<f32> = (0..clusters * dimension).map(|_| rng.next()).collect();
    let mut sample = |group: usize| -> Vec<f32> {
        let mut vector: Vec<_> = (0..dimension)
            .map(|d| {
                if clusters == 0 {
                    rng.next()
                } else {
                    centers[group * dimension + d] + 0.3 * rng.next()
                }
            })
            .collect();
        normalize(&mut vector).unwrap();
        vector
    };
    let mut matrix = Vec::with_capacity(rows * dimension);
    for row in 0..rows {
        matrix.extend(sample(row * clusters / rows));
    }
    let queries = (0..100)
        .map(|i| sample(if clusters == 0 { 0 } else { i % clusters }))
        .collect();
    (matrix, queries)
}
pub fn repair_checksums(bytes: &mut [u8]) {
    use sha2::{Digest, Sha256};
    let payload = Sha256::digest(&bytes[256..]);
    bytes[144..176].copy_from_slice(&payload);
    bytes[176..208].fill(0);
    let header = Sha256::digest(&bytes[..256]);
    bytes[176..208].copy_from_slice(&header);
}
pub fn read64(bytes: &[u8], at: usize) -> usize {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize
}
pub fn assert_same_inode_contents(path: &Path, before: &[u8]) {
    assert_eq!(fs::read(path).unwrap(), before);
}
