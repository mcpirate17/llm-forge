#![cfg(feature = "vector-search")]
#[path = "support/vector_fixture.rs"]
mod support;
use conductor_native::vector_index::{Builder, Config, Query, QueryWorkspace, Snapshot};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use support::*;

struct Count;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Count {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }
}
#[global_allocator]
static ALLOCATOR: Count = Count;

#[test]
fn successful_mapped_search_and_exact_reranking_allocate_nothing_after_preparation() {
    let (matrix, queries) = dataset(1024, 64, 8, 662);
    let index = Builder::new(space(64), matrix, Config::default()).unwrap();
    let fixture = Fixture::new();
    let path = fixture.path("index.bin");
    index.write(&path, [1; 32]).unwrap();
    let mapped = Snapshot::open(&path).unwrap();
    let prepared: Vec<_> = queries
        .into_iter()
        .map(|q| Query::new(space(64), q).unwrap())
        .collect();
    let mut scratch = QueryWorkspace::new(mapped.len(), 200).unwrap();
    mapped.search(&prepared[0], 10, 160, &mut scratch).unwrap();
    ENABLED.store(true, Ordering::SeqCst);
    for query in &prepared {
        let result = mapped.search(query, 10, 160, &mut scratch).unwrap();
        for hit in result.hits {
            std::hint::black_box(mapped.score(query, hit.row as usize).unwrap());
        }
        std::hint::black_box(mapped.exact(query, 10, &mut scratch).unwrap());
    }
    ENABLED.store(false, Ordering::SeqCst);
    assert_eq!(ALLOCATIONS.load(Ordering::SeqCst), 0);
}
