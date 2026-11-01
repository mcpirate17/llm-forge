//! Counts allocations on the measured test thread only.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
struct CountingAllocator;
fn count() {
    if ENABLED.try_with(Cell::get).unwrap_or(false) {
        ALLOCATIONS.with(|value| value.set(value.get() + 1));
    }
}
// SAFETY: the system allocator receives the exact original pointer and layout.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;
struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        ENABLED.with(|flag| flag.set(false));
    }
}
pub fn measure<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|n| n.set(0));
    ENABLED.with(|flag| flag.set(true));
    let guard = Guard;
    let result = f();
    drop(guard);
    (result, ALLOCATIONS.with(Cell::get))
}
