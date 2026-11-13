//! Allocation. M0 uses a "NoGC" heap: bump allocation from large zeroed
//! chunks that are never freed. MMTk replaces this in M1 (runtime.md §4).
//!
//! Invariant kept from day one: **allocation never collects**. A GC can only
//! happen at a safepoint, where every live Loom value is in a heap stack frame.
//! Rust primitives are leaves, so they can hold raw `Value`s across allocation
//! (runtime.md §4.3).

use std::alloc::{Layout, alloc_zeroed, handle_alloc_error};
use std::sync::atomic::{AtomicUsize, Ordering};

static HEAP_BYTES: AtomicUsize = AtomicUsize::new(0);

const CHUNK_BYTES: usize = 4 << 20;
const LARGE_BYTES: usize = 256 << 10;

/// Total bytes obtained from the OS so far.
pub fn heap_bytes() -> usize {
    HEAP_BYTES.load(Ordering::Relaxed)
}

fn chunk(bytes: usize) -> *mut u64 {
    let layout = Layout::from_size_align(bytes, 16).expect("bad chunk size");
    // SAFETY: layout has non-zero size.
    let p = unsafe { alloc_zeroed(layout) };
    if p.is_null() {
        handle_alloc_error(layout);
    }
    HEAP_BYTES.fetch_add(bytes, Ordering::Relaxed);
    p as *mut u64
}

/// Per-carrier allocation state (thread-local allocation buffer).
pub struct Mutator {
    cur: usize,
    end: usize,
}

impl Default for Mutator {
    fn default() -> Self {
        Self::new()
    }
}

impl Mutator {
    pub const fn new() -> Self {
        Mutator { cur: 0, end: 0 }
    }

    /// Allocate `words` 8-byte words, 8-byte aligned and zeroed.
    #[inline]
    pub fn alloc_words(&mut self, words: usize) -> *mut u64 {
        let bytes = words * 8;
        if self.end - self.cur >= bytes {
            let p = self.cur;
            self.cur += bytes;
            return p as *mut u64;
        }
        self.alloc_slow(bytes)
    }

    #[cold]
    fn alloc_slow(&mut self, bytes: usize) -> *mut u64 {
        if bytes >= LARGE_BYTES {
            return chunk(bytes);
        }
        let c = chunk(CHUNK_BYTES);
        self.cur = c as usize + bytes;
        self.end = c as usize + CHUNK_BYTES;
        c
    }
}
