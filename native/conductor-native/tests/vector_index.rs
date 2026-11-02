#![cfg(feature = "vector-search")]
#[path = "support/vector_fixture.rs"]
mod support;
use conductor_native::vector_index::{Builder, Config, Query, QueryWorkspace, Snapshot};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::Arc;
use support::*;

#[test]
fn mapped_hnsw_recall_against_exact_unseen_queries_for_uniform_and_clustered_corpora() {
    for clusters in [0, 16] {
        let (matrix, queries) = dataset(4096, 64, clusters, 7621);
        let builder = Builder::new(space(64), matrix, Config::default()).unwrap();
        let fixture = Fixture::new();
        let path = fixture.path("index.bin");
        builder.write(&path, [1; 32]).unwrap();
        let mapped = Snapshot::open(&path).unwrap();
        let mut scratch = QueryWorkspace::new(mapped.len(), 200).unwrap();
        let mut matched = 0;
        let mut distances = 0;
        for raw in queries {
            let query = Query::new(space(64), raw).unwrap();
            let expected = builder
                .exact(&query, 10, &mut scratch)
                .unwrap()
                .hits
                .to_vec();
            assert_eq!(
                mapped.exact(&query, 10, &mut scratch).unwrap().hits,
                expected
            );
            let result = mapped.search(&query, 10, 160, &mut scratch).unwrap();
            matched += result
                .hits
                .iter()
                .filter(|hit| expected.iter().any(|e| e.row == hit.row))
                .count();
            distances += result.distance_evaluations;
        }
        let recall = matched as f64 / 1000.0;
        eprintln!(
            "clusters={clusters}, recall@10={recall}, mean_distances={}",
            distances / 100
        );
        assert!(recall >= 0.95, "recall@10={recall} clusters={clusters}");
        assert!(
            distances < 100 * mapped.len(),
            "HNSW should inspect fewer vectors on this fixture"
        );
    }
}

#[test]
fn exact_scores_match_independent_reference_and_ties_are_stable() {
    let matrix = vec![1.0, 0.0, 1.0, 0.0, 0.0, 1.0, -1.0, 0.0];
    let builder = Builder::new(space(2), matrix.clone(), Config::default()).unwrap();
    let query = Query::new(space(2), vec![1.0, 0.0]).unwrap();
    let mut scratch = QueryWorkspace::new(4, 4).unwrap();
    let result = builder.exact(&query, 4, &mut scratch).unwrap();
    assert_eq!(
        result.hits.iter().map(|h| h.row).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    for hit in result.hits {
        let row = &matrix[hit.row as usize * 2..][..2];
        let independent =
            f64::from(row[0]) / (f64::from(row[0]).powi(2) + f64::from(row[1]).powi(2)).sqrt();
        assert!((hit.cosine - independent).abs() < 1e-12);
    }
    assert_eq!(
        builder.search(&query, 4, 4, &mut scratch).unwrap().hits[0].row,
        0
    );
    let fixture = Fixture::new();
    let path = fixture.path("index.bin");
    builder.write(&path, [3; 32]).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert!(builder.write(&path, [3; 32]).is_err());
    assert_same_inode_contents(&path, &bytes);
    builder
        .write(&fixture.path("identical.bin"), [3; 32])
        .unwrap();
    assert_eq!(fs::read(fixture.path("identical.bin")).unwrap(), bytes);
    let snapshot = Snapshot::open(&path).unwrap();
    assert_eq!(snapshot.space(), &space(2));
    assert_eq!(snapshot.source_sha256(), [3; 32]);
    let mut wrong = space(2);
    wrong.model_sha256[0] ^= 1;
    assert!(snapshot
        .search(
            &Query::new(wrong, vec![1.0, 0.0]).unwrap(),
            1,
            4,
            &mut scratch
        )
        .is_err());
    assert!(snapshot.score(&query, 4).is_err());
}

#[test]
fn snapshot_rejects_corruption_including_valid_checksums_over_invalid_shapes() {
    let fixture = Fixture::new();
    let path = fixture.path("original.bin");
    Builder::new(
        space(2),
        vec![1.0, 0.0, 0.0, 1.0, -1.0, 0.0],
        Config::default(),
    )
    .unwrap()
    .write(&path, [1; 32])
    .unwrap();
    let bytes = fs::read(&path).unwrap();
    let table = read64(&bytes, 56);
    let graph = read64(&bytes, 64);
    let mut corruptions = Vec::new();
    for at in [0, 20, 80, 112, 144, 176, 220, 256, bytes.len() - 1] {
        let mut bad = bytes.clone();
        bad[at] ^= 1;
        corruptions.push(bad);
    }
    for (at, value) in [
        (16, 0u32),
        (20, u32::MAX),
        (24, 0),
        (32, 99),
        (36, 17),
        (graph, 0),
        (graph + 4, 999),
        (graph + 8, 999),
    ] {
        let mut bad = bytes.clone();
        bad[at..at + 4].copy_from_slice(&value.to_le_bytes());
        repair_checksums(&mut bad);
        corruptions.push(bad);
    }
    for (at, value) in [
        (table, 0u64),
        (table + 8, (graph + 1) as u64),
        (table + 8, u64::MAX),
    ] {
        let mut bad = bytes.clone();
        bad[at..at + 8].copy_from_slice(&value.to_le_bytes());
        repair_checksums(&mut bad);
        corruptions.push(bad);
    }
    let mut nan = bytes.clone();
    nan[256..260].copy_from_slice(&f32::NAN.to_le_bytes());
    repair_checksums(&mut nan);
    corruptions.push(nan);
    for (index, bad) in corruptions.iter().enumerate() {
        assert!(
            Snapshot::open(&fixture.write(&format!("bad-{index}"), bad)).is_err(),
            "accepted corruption {index}"
        );
    }
    for length in [0, 1, 255, 256, bytes.len() - 1] {
        assert!(
            Snapshot::open(&fixture.write(&format!("short-{length}"), &bytes[..length])).is_err()
        );
    }
    symlink(&path, fixture.path("symlink")).unwrap();
    assert!(Snapshot::open(&fixture.path("symlink")).is_err());
    fs::hard_link(&path, fixture.path("hardlink")).unwrap();
    assert!(Snapshot::open(&path).is_err());
    fs::remove_file(fixture.path("hardlink")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(Snapshot::open(&path).is_err());
}

#[test]
fn readers_pin_generations_across_atomic_replacement_and_unlink() {
    let fixture = Fixture::new();
    let path = fixture.path("current.bin");
    Builder::new(space(2), vec![1.0, 0.0], Config::default())
        .unwrap()
        .write(&path, [1; 32])
        .unwrap();
    let old = Arc::new(Snapshot::open(&path).unwrap());
    let readers: Vec<_> = (0..8)
        .map(|_| {
            let snapshot = old.clone();
            std::thread::spawn(move || {
                let query = Query::new(space(2), vec![1.0, 0.0]).unwrap();
                let mut scratch = QueryWorkspace::new(1, 1).unwrap();
                for _ in 0..1000 {
                    assert_eq!(
                        snapshot.search(&query, 1, 1, &mut scratch).unwrap().hits[0].cosine,
                        1.0
                    );
                }
            })
        })
        .collect();
    let next = fixture.path("next.bin");
    Builder::new(space(2), vec![-1.0, 0.0], Config::default())
        .unwrap()
        .write(&next, [2; 32])
        .unwrap();
    fs::rename(&next, &path).unwrap();
    let new = Snapshot::open(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert_eq!(new.source_sha256(), [2; 32]);
    assert_eq!(old.source_sha256(), [1; 32]);
    let query = Query::new(space(2), vec![1.0, 0.0]).unwrap();
    assert_eq!(new.score(&query, 0).unwrap(), -1.0);
    for reader in readers {
        reader.join().unwrap();
    }
}

#[test]
fn invalid_shapes_and_nonfinite_or_zero_vectors_fail_before_construction() {
    for values in [
        vec![0.0, 0.0],
        vec![f32::NAN, 1.0],
        vec![f32::INFINITY, 0.0],
        vec![1.0],
    ] {
        assert!(Builder::new(space(2), values, Config::default()).is_err());
    }
    assert!(Builder::new(space(0), vec![], Config::default()).is_err());
    assert!(Builder::new(
        space(1),
        vec![1.0],
        Config {
            neighbors: 3,
            ..Config::default()
        }
    )
    .is_err());
    assert!(Builder::new(
        space(1),
        vec![1.0],
        Config {
            ef_construction: 2,
            ..Config::default()
        }
    )
    .is_err());
    assert!(Query::new(space(2), vec![1.0]).is_err());
    let fixture = Fixture::new();
    assert!(Builder::new(space(1), vec![1.0], Config::default())
        .unwrap()
        .write(&fixture.path("bad"), [0; 32])
        .is_err());
    assert!(!fixture.path("bad").exists());
}

#[test]
fn empty_snapshots_are_queryable_and_still_check_their_offset_table() {
    let fixture = Fixture::new();
    let path = fixture.path("empty.bin");
    let builder = Builder::new(space(2), Vec::new(), Config::default()).unwrap();
    assert!(builder.is_empty());
    builder.write(&path, [1; 32]).unwrap();
    let snapshot = Snapshot::open(&path).unwrap();
    assert!(snapshot.is_empty());
    let mut workspace = QueryWorkspace::new(0, 8).unwrap();
    let query = Query::new(space(2), vec![1.0, 0.0]).unwrap();
    assert!(snapshot
        .exact(&query, 5, &mut workspace)
        .unwrap()
        .hits
        .is_empty());
    assert!(snapshot
        .search(&query, 5, 8, &mut workspace)
        .unwrap()
        .hits
        .is_empty());
    assert!(snapshot.score(&query, 0).is_err());
    let mut bytes = fs::read(path).unwrap();
    bytes[256] ^= 1;
    repair_checksums(&mut bytes);
    assert!(Snapshot::open(&fixture.write("bad-empty.bin", &bytes)).is_err());
}

#[test]
fn reused_rows_preserve_bits_and_reject_invalid_masks_or_vectors() {
    let original = vec![0.6_f32, 0.8_f32, 4.0, 3.0];
    let expected: Vec<_> = original[..2].iter().map(|x| x.to_bits()).collect();
    let builder = Builder::new_reusing_checked(
        space(2),
        original,
        Config::default(),
        &[true, false],
        || Ok(()),
    )
    .unwrap();
    let fixture = Fixture::new();
    let path = fixture.path("reuse.bin");
    builder.write(&path, [1; 32]).unwrap();
    let mapped = Snapshot::open(&path).unwrap();
    assert_eq!(
        mapped
            .row(0)
            .unwrap()
            .iter()
            .map(|x| x.to_bits())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(mapped.row(2).is_err());
    assert!(
        Builder::new_reusing_checked(space(2), vec![1.0, 0.0], Config::default(), &[], || Ok(()))
            .is_err()
    );
    assert!(Builder::new_reusing_checked(
        space(2),
        vec![3.0, 4.0],
        Config::default(),
        &[true],
        || Ok(())
    )
    .is_err());
    assert!(Builder::new_reusing_checked(
        space(2),
        vec![f32::NAN, 0.0],
        Config::default(),
        &[true],
        || Ok(())
    )
    .is_err());
}
