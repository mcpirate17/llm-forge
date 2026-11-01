use anyhow::{bail, Result};
use std::sync::OnceLock;

/// Normalize with f64 accumulation, including subnormal and very large f32 inputs.
pub fn normalize(vector: &mut [f32]) -> Result<()> {
    if vector.is_empty() || vector.iter().any(|x| !x.is_finite()) {
        bail!("embedding must be nonempty and finite");
    }
    let squared: f64 = vector.iter().map(|&x| f64::from(x).powi(2)).sum();
    if squared == 0.0 {
        bail!("zero embedding has no cosine direction");
    }
    let norm = squared.sqrt();
    for x in vector {
        *x = (f64::from(*x) / norm) as f32;
    }
    Ok(())
}

type Dot = fn(&[f32], &[f32]) -> f64;
fn backend() -> (Dot, &'static str) {
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
        return (checked_avx2, "avx2-fma-f64");
    }
    (scalar, "scalar-f64")
}
fn selected() -> &'static (Dot, &'static str) {
    static DOT: OnceLock<(Dot, &'static str)> = OnceLock::new();
    DOT.get_or_init(backend)
}
pub fn score_backend() -> &'static str {
    selected().1
}
pub(crate) fn dot(left: &[f32], right: &[f32]) -> f64 {
    assert_eq!(
        left.len(),
        right.len(),
        "internal cosine dimension mismatch"
    );
    (selected().0)(left, right).clamp(-1.0, 1.0)
}
fn scalar(left: &[f32], right: &[f32]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(&a, &b)| f64::from(a) * f64::from(b))
        .sum()
}

#[cfg(target_arch = "x86_64")]
fn checked_avx2(left: &[f32], right: &[f32]) -> f64 {
    // Selected only after runtime feature detection; dot checks equal lengths.
    unsafe { avx2(left, right) }
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn avx2(left: &[f32], right: &[f32]) -> f64 {
    use std::arch::x86_64::*;
    let mut sums = [_mm256_setzero_pd(); 4];
    let end = left.len() / 16 * 16;
    for base in (0..end).step_by(16) {
        for (lane, sum) in sums.iter_mut().enumerate() {
            let offset = base + lane * 4;
            // Each unaligned load is within the equal-length input slices.
            let a = unsafe { _mm_loadu_ps(left.as_ptr().add(offset)) };
            let b = unsafe { _mm_loadu_ps(right.as_ptr().add(offset)) };
            *sum = _mm256_fmadd_pd(_mm256_cvtps_pd(a), _mm256_cvtps_pd(b), *sum);
        }
    }
    let sum = _mm256_add_pd(
        _mm256_add_pd(sums[0], sums[1]),
        _mm256_add_pd(sums[2], sums[3]),
    );
    let mut lanes = [0.0f64; 4];
    unsafe { _mm256_storeu_pd(lanes.as_mut_ptr(), sum) };
    lanes.iter().sum::<f64>() + scalar(&left[end..], &right[end..])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dispatched_simd_matches_independent_f64_with_unaligned_tails() {
        for size in [1, 3, 15, 16, 17, 32, 193, 768, 8192] {
            let mut a: Vec<_> = (0..size + 1)
                .map(|i| ((i * 7919 % 65521) as f32 - 32760.0) / 32760.0)
                .collect();
            let mut b: Vec<_> = (0..size + 1)
                .map(|i| ((i * 997 % 65521) as f32 - 32760.0) / 32760.0)
                .collect();
            normalize(&mut a[1..]).unwrap();
            normalize(&mut b[1..]).unwrap();
            let expected = scalar(&a[1..], &b[1..]);
            assert!((dot(&a[1..], &b[1..]) - expected).abs() < 1e-12);
        }
        for mut vector in [vec![], vec![0.0], vec![f32::NAN], vec![f32::INFINITY]] {
            assert!(normalize(&mut vector).is_err());
        }
        for value in [f32::MAX, f32::from_bits(1)] {
            let mut vector = [value, value];
            normalize(&mut vector).unwrap();
            assert!((scalar(&vector, &vector) - 1.0).abs() < 1e-6);
        }
    }
}
