//! The heap: an MMTk binding (runtime.md §4).
//!
//! **Plan.** Immix built with `immix_non_moving`: parallel, stop-the-world,
//! and objects never move. Nothing in the runtime has to update raw pointers
//! after a GC (the interpreter caches code and frame addresses). The
//! generational StickyImmix plan (`sticky_immix_non_moving_nursery`) needs
//! write barriers on mutable heap stores first.
//!
//! **Allocation never collects.** Allocation uses MMTk's over-commit and
//! never-block options. When the heap is full MMTk *requests* a collection,
//! and its workers stop the world through our safepoint protocol, so the GC
//! only ever sees threads parked at safepoints with exact stacks.
//!
//! **Roots** are the runtime tables (symbols, keywords, namespaces,
//! singletons) plus whatever the VM registers with `set_extra_roots`
//! (run queues, timers, running fibers). Fiber stacks are scanned as part of
//! their fiber object, up to each segment's live top.

use crate::clos::{class, generic};
use crate::conc::{chan, fiber, scope, waiter};
use crate::func::SEG_HEADER;
use crate::object::{Kind, Layout};
use crate::value::{TAG_CONS, TAG_MASK, TAG_OBJ, Value};
use mmtk::Plan;
use mmtk::util::alloc::AllocationOptions;
use mmtk::util::constants::LOG_BYTES_IN_PAGE;
use mmtk::util::copy::{CopySemantics, GCWorkerCopyContext};
use mmtk::util::heap::{GCTriggerPolicy, SpaceStats};
use mmtk::util::opaque_pointer::{OpaquePointer, VMMutatorThread, VMThread, VMWorkerThread};
use mmtk::util::options::{GCTriggerSelector, PlanSelector};
use mmtk::util::{Address, ObjectReference};
use mmtk::vm::slot::{Slot, UnimplementedMemorySlice};
use mmtk::vm::{
    ActivePlan, Collection, Finalizable, GCThreadContext, ObjectModel, ReferenceGlue, RootsWorkFactory, Scanning, SlotVisitor, VMBinding,
    VMGlobalLogBitSpec, VMLocalForwardingBitsSpec, VMLocalForwardingPointerSpec, VMLocalLOSMarkNurserySpec, VMLocalMarkBitSpec,
};
use mmtk::{AllocationSemantics, MMTK, MMTKBuilder, memory_manager};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

// ---- the binding type ------------------------------------------------------------

#[derive(Default)]
pub struct Skein;

impl VMBinding for Skein {
    type VMObjectModel = Skein;
    type VMScanning = Skein;
    type VMCollection = Skein;
    type VMActivePlan = Skein;
    type VMReferenceGlue = Skein;
    type VMSlot = SkeinSlot;
    type VMMemorySlice = UnimplementedMemorySlice<SkeinSlot>;
    const MIN_ALIGNMENT: usize = 8;
    const MAX_ALIGNMENT: usize = 8;
}

static MMTK_INSTANCE: OnceLock<&'static MMTK<Skein>> = OnceLock::new();
static LOS_THRESHOLD: AtomicUsize = AtomicUsize::new(usize::MAX);
static GC_COUNT: AtomicU64 = AtomicU64::new(0);
/// Objects collections have moved (0 in a build that never moves any).
static MOVED: AtomicU64 = AtomicU64::new(0);
/// How long collections have stopped the world, in nanoseconds: in all,
/// the longest, and the last (gc-stats). A pause is from the stop being
/// asked for to the carriers being let go again: what a program waits.
static PAUSE_TOTAL: AtomicU64 = AtomicU64::new(0);
static PAUSE_MAX: AtomicU64 = AtomicU64::new(0);
static PAUSE_LAST: AtomicU64 = AtomicU64::new(0);
static PAUSE_BEGAN: Mutex<Option<std::time::Instant>> = Mutex::new(None);
/// SKEIN_GC_LOG=1: a line on standard error for each collection (as Go's
/// gctrace): how long it stopped the program, the heap before and after,
/// and how far the heap may grow before the next.
static GC_LOG: AtomicUsize = AtomicUsize::new(0);
static PAGES_BEFORE: AtomicUsize = AtomicUsize::new(0);
static PAGES_AFTER: AtomicUsize = AtomicUsize::new(0);
static PAGES_LIVE: AtomicUsize = AtomicUsize::new(0);
static PAGES_LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PAGES_LIMIT: AtomicUsize = AtomicUsize::new(0);

fn mmtk() -> &'static MMTK<Skein> {
    MMTK_INSTANCE.get().expect("heap not initialised")
}

fn env_bytes(name: &str) -> Option<usize> {
    let s = std::env::var(name).ok()?;
    let (num, mult) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1 << 10),
        'm' | 'M' => (&s[..s.len() - 1], 1 << 20),
        'g' | 'G' => (&s[..s.len() - 1], 1 << 30),
        _ => (&s[..], 1),
    };
    num.parse::<usize>().ok().map(|n| n * mult)
}

// ---- when to collect ------------------------------------------------------------------
//
// After each collection the heap may grow by what's live times
// SKEIN_HEAP_GROWTH percent, 100 by default: a collection when the program
// has taken as much new memory as it had live data, as Go's GOGC=100;
// within SKEIN_HEAP_MIN (64 MB) and SKEIN_HEAP_MAX (8 GB).
//
// "What's live" is the bytes of the objects the collection found alive,
// not the pages the heap holds after it. The collector doesn't move
// objects, so a page stays in use while any object on it lives, and a
// program that keeps one new object in every few KB it allocates (a store
// taking writes) leaves a little live data on a great many pages. Sized by
// pages, as it was until 2026-11-18, the heap fed itself: more room before
// the next collection spread the survivors over more pages, which counted
// as more live data, which made more room. stash's benchmark, with 1.3 GB
// in use after loading, held 7 GB after 40 s and stopped only at
// SKEIN_HEAP_MAX, its pauses at half a second. Sized by live bytes, the
// room is the same each time, and what's free on the pages in use (their
// holes cost no new pages) is filled before new pages are taken.
//
// MMTk's own dynamic size (MemBalancer, SKEIN_HEAP_GROWTH=balancer) sizes
// the room above live data by how fast collections trace against how fast
// the program allocates; Skein traces fast and allocates a lot, so it left
// a few MB above live data, and a program with 65 MB live collected every
// few MB it allocated: linting a 34 KB file took about 100 collections and
// 550 ms, against 12 or so and 150 ms now.

static HEAP_MIN_PAGES: AtomicUsize = AtomicUsize::new(0);
static HEAP_MAX_PAGES: AtomicUsize = AtomicUsize::new(0);
static HEAP_GROWTH: AtomicUsize = AtomicUsize::new(100);
/// The heap's size now, in pages: a collection when it's full. Set as a
/// collection ends (`grow_heap`).
static HEAP_LIMIT_PAGES: AtomicUsize = AtomicUsize::new(0);
/// Pages that allocations waiting on this collection need.
static PENDING_PAGES: AtomicUsize = AtomicUsize::new(0);
/// With generations: the nursery's size in pages (0: no generations), and
/// how far what's in use may grow before a full collection (`grow_heap`).
/// SKEIN_GC_LOG=2: where the last pause went. Nanoseconds to stop the
/// carriers, to list the roots (and how many), and in the weak references.
static PHASE_STOP: AtomicU64 = AtomicU64::new(0);
static PHASE_ROOTS: AtomicU64 = AtomicU64::new(0);
static PHASE_ROOT_COUNT: AtomicUsize = AtomicUsize::new(0);
static PHASE_WEAK: AtomicU64 = AtomicU64::new(0);
static NURSERY_PAGES: AtomicUsize = AtomicUsize::new(0);
static MATURE_LIMIT_PAGES: AtomicUsize = AtomicUsize::new(0);
/// What was in use after the last full collection: the budget for what's
/// old is from here to MATURE_LIMIT_PAGES (`idle_collection_due`).
static MATURE_BASE_PAGES: AtomicUsize = AtomicUsize::new(0);

struct ProportionalTrigger;

/// A collection has ended: how large the heap may get before the next.
/// The pages in use now, and room for as much again as is live (times the
/// growth percent). Called from `resume_mutators`, by when MMTk has counted
/// the live bytes (it hasn't yet in the trigger's own `on_gc_end`).
fn grow_heap(mmtk: &'static MMTK<Skein>) {
    let in_use = mmtk.get_plan().get_reserved_pages();
    // With generations (M11 part 4) there are two sizes. Young collections
    // come as the nursery fills (SKEIN_GC_NURSERY), so the heap's limit
    // only has to leave a nursery's room above what's in use: set after
    // every collection. What grows from one young collection to the next
    // is what they promote and what dies old, and that has a budget, set
    // by each full collection as the heap's size always was (what's in use
    // and as much again as is live): once it's spent, the next collection
    // is a full one. (With one limit for both, the heap sat just under it
    // and a young collection ran every few MB.)
    let nursery = NURSERY_PAGES.load(Ordering::Relaxed);
    let young = mmtk.get_plan().generational().is_some_and(|g| g.is_current_gc_nursery());
    if nursery != 0 && young && MATURE_LIMIT_PAGES.load(Ordering::Relaxed) != 0 {
        // (The full collection comes straight after this young one, not a
        // nursery later: a full collection marks what's young where it
        // lies, and a nursery of new objects each left alone on its block
        // is the fragmentation over again, now among the old.)
        let spent = in_use >= MATURE_LIMIT_PAGES.load(Ordering::Relaxed);
        if spent && let Some(g) = mmtk.get_plan().generational() {
            g.force_full_heap_collection();
        }
        let room = if spent { 256 } else { nursery + nursery / 4 };
        let limit = (in_use + room).min(HEAP_MAX_PAGES.load(Ordering::Relaxed));
        HEAP_LIMIT_PAGES.store(limit, Ordering::Relaxed);
        PAGES_AFTER.store(in_use, Ordering::Relaxed);
        PAGES_LIMIT.store(limit, Ordering::Relaxed);
        return;
    }
    let spaces = memory_manager::live_bytes_in_last_gc(mmtk);
    let live_bytes: usize = spaces.values().map(|s| s.live_bytes).sum();
    if GC_LOG.load(Ordering::Relaxed) > 1 {
        // (SKEIN_GC_LOG=2: each space's pages in use and the bytes live on them)
        let mut names: Vec<_> = spaces.iter().collect();
        names.sort_by_key(|(n, _)| **n);
        for (name, st) in names {
            eprintln!("  {name}: {} MB in use, {} MB live", (st.used_pages << LOG_BYTES_IN_PAGE) >> 20, st.live_bytes >> 20);
        }
    }
    // (never more than the pages in use: the count is of objects, without
    // what's lost between them)
    let live = (live_bytes >> LOG_BYTES_IN_PAGE).min(in_use);
    let growth = HEAP_GROWTH.load(Ordering::Relaxed);
    let want = in_use + live / 100 * growth + PENDING_PAGES.swap(0, Ordering::SeqCst);
    let (min, max) = (HEAP_MIN_PAGES.load(Ordering::Relaxed), HEAP_MAX_PAGES.load(Ordering::Relaxed));
    if nursery != 0 {
        // (a full collection, with generations: the budget for what's
        // old, and a nursery's room for what's new)
        MATURE_LIMIT_PAGES.store(want.clamp(min, max), Ordering::Relaxed);
        MATURE_BASE_PAGES.store(in_use, Ordering::Relaxed);
        let limit = (in_use + nursery + nursery / 4).clamp(min, max);
        HEAP_LIMIT_PAGES.store(limit, Ordering::Relaxed);
        PAGES_AFTER.store(in_use, Ordering::Relaxed);
        PAGES_LIVE.store(live, Ordering::Relaxed);
        PAGES_LIVE_BYTES.store(live_bytes, Ordering::Relaxed);
        PAGES_LIMIT.store(want.clamp(min, max), Ordering::Relaxed);
        return;
    }
    HEAP_LIMIT_PAGES.store(want.clamp(min, max), Ordering::Relaxed);
    PAGES_AFTER.store(in_use, Ordering::Relaxed);
    PAGES_LIVE.store(live, Ordering::Relaxed);
    PAGES_LIVE_BYTES.store(live_bytes, Ordering::Relaxed);
    PAGES_LIMIT.store(want.clamp(min, max), Ordering::Relaxed);
}

impl GCTriggerPolicy<Skein> for ProportionalTrigger {
    fn on_pending_allocation(&self, pages: usize) {
        PENDING_PAGES.fetch_add(pages, Ordering::SeqCst);
    }
    fn on_gc_end(&self, _: &'static MMTK<Skein>) {
        // (the size is set in resume_mutators: grow_heap)
    }
    fn is_gc_required(&self, space_full: bool, space: Option<SpaceStats<Skein>>, plan: &dyn Plan<VM = Skein>) -> bool {
        plan.collection_required(space_full, space)
    }
    fn is_heap_full(&self, plan: &dyn Plan<VM = Skein>) -> bool {
        plan.get_reserved_pages() > HEAP_LIMIT_PAGES.load(Ordering::Relaxed)
    }
    fn get_current_heap_size_in_pages(&self) -> usize {
        HEAP_LIMIT_PAGES.load(Ordering::Relaxed)
    }
    fn get_max_heap_size_in_pages(&self) -> usize {
        HEAP_MAX_PAGES.load(Ordering::Relaxed)
    }
    fn can_heap_size_grow(&self) -> bool {
        HEAP_LIMIT_PAGES.load(Ordering::Relaxed) < HEAP_MAX_PAGES.load(Ordering::Relaxed)
    }
}

/// Initialise MMTk (once per process). Collection stays disabled until
/// `enable_collection`, so boot code needs no safepoints.
pub fn init_heap() {
    let m = *MMTK_INSTANCE.get_or_init(|| {
        let mut b = MMTKBuilder::new();
        // The plan (M11): generations, the young copied (MMTk's StickyImmix),
        // in a build that moves objects, which is the default one. Without
        // generations (SKEIN_GC_PLAN=immix), and in a build with the
        // `non-moving` feature, it's Immix alone, as it was before M11.
        // (SKEIN_GC_PLAN=marksweep: MMTk's free-list plan, to compare.
        // SKEIN_GC_PLAN=sticky in a non-moving build: generations with
        // nothing copied.)
        let asked = std::env::var("SKEIN_GC_PLAN");
        let plan = match asked.as_deref() {
            Ok("marksweep") => PlanSelector::MarkSweep,
            Ok("immix") => PlanSelector::Immix,
            Ok("sticky") => PlanSelector::StickyImmix,
            Err(_) if !cfg!(feature = "non-moving") => PlanSelector::StickyImmix,
            _ => PlanSelector::Immix,
        };
        let plan = match plan {
            // (generations need the write barrier)
            PlanSelector::StickyImmix => {
                BARRIER.store(true, Ordering::Relaxed);
                // The nursery: how much may be allocated before a young
                // collection (SKEIN_GC_NURSERY, 256 MB), and the least room
                // under the heap's limit that leaves the next collection a
                // young one (2 MB). MMTk's own is a quarter of the heap for
                // the least, which the heap's growth never leaves free: all
                // but a few collections were full ones.
                let max = env_bytes("SKEIN_GC_NURSERY").unwrap_or(256 << 20).max(4 << 20);
                NURSERY_PAGES.store(max >> LOG_BYTES_IN_PAGE, Ordering::Relaxed);
                b.options.nursery.set(mmtk::util::options::NurserySize::Bounded { min: 2 << 20, max });
                PlanSelector::StickyImmix
            }
            other => other,
        };
        b.options.plan.set(plan);
        // (the live bytes each collection finds: what the heap's growth goes by)
        b.options.count_live_bytes_in_gc.set(true);
        // (SKEIN_GC_MOVE=stress, in a build that moves objects: every
        // collection defragments, and every block is one to move objects
        // out of, so that a pointer kept where the collector can't rewrite
        // it goes wrong at once and not one run in a thousand)
        if moves_objects() {
            match std::env::var("SKEIN_GC_MOVE").as_deref() {
                Ok("stress") => {
                    b.options.immix_always_defrag.set(true);
                    b.options.immix_defrag_every_block.set(true);
                    b.options.immix_defrag_headroom_percent.set(50);
                }
                // (every collection defragments, choosing its blocks as MMTk
                // does: the sparsest first, as far as the headroom goes)
                Ok("always") => {
                    b.options.immix_always_defrag.set(true);
                }
                _ => {}
            }
            // (SKEIN_GC_HEADROOM: the percent of the heap kept to move
            // objects into; MMTk's own is 2)
            if let Some(p) = std::env::var("SKEIN_GC_HEADROOM").ok().and_then(|p| p.parse::<usize>().ok()).filter(|p| *p <= 50) {
                b.options.immix_defrag_headroom_percent.set(p);
            }
        }
        let max = env_bytes("SKEIN_HEAP_MAX").unwrap_or(8 << 30);
        let min = env_bytes("SKEIN_HEAP_MIN").unwrap_or(64 << 20).min(max);
        let growth = std::env::var("SKEIN_HEAP_GROWTH").ok();
        if let Ok(v) = std::env::var("SKEIN_GC_LOG")
            && !v.is_empty()
            && v != "0"
        {
            GC_LOG.store(if v == "2" { 2 } else { 1 }, Ordering::Relaxed);
        }
        if growth.as_deref() == Some("balancer") {
            b.options.gc_trigger.set(GCTriggerSelector::DynamicHeapSize(min, max));
        } else {
            HEAP_MIN_PAGES.store(min >> LOG_BYTES_IN_PAGE, Ordering::Relaxed);
            HEAP_MAX_PAGES.store(max >> LOG_BYTES_IN_PAGE, Ordering::Relaxed);
            if let Some(g) = growth.and_then(|g| g.parse::<usize>().ok()) {
                HEAP_GROWTH.store(g, Ordering::Relaxed);
            }
            b.options.gc_trigger.set(GCTriggerSelector::Delegated);
        }
        let m: &'static MMTK<Skein> = Box::leak(memory_manager::mmtk_init::<Skein>(&b));
        LOS_THRESHOLD.store(m.get_plan().constraints().max_non_los_default_alloc_bytes, Ordering::Relaxed);
        m
    });
    // Start the GC workers (after the instance is published: they look it
    // up), but ignore collection requests until the VM has its safepoints
    // and roots in place (`enable_collection`).
    static WORKERS: std::sync::Once = std::sync::Once::new();
    WORKERS.call_once(|| {
        memory_manager::initialize_collection(m, VMThread::UNINITIALIZED);
        let _ = memory_manager::disable_collection(m);
    });
}

/// Let MMTk collect from now on.
pub fn enable_collection() {
    init_heap();
    memory_manager::enable_collection(mmtk());
}

/// Bytes currently in use by the heap.
pub fn heap_bytes() -> usize {
    MMTK_INSTANCE.get().map(|m| memory_manager::used_bytes(m)).unwrap_or(0)
}

/// Does this build's collector move objects? (Not with the `non-moving`
/// feature, the default.)
pub fn moves_objects() -> bool {
    !cfg!(feature = "non-moving")
}

/// How many objects collections have moved so far.
pub fn moved_objects() -> u64 {
    MOVED.load(Ordering::Relaxed)
}

/// Number of completed collections.
pub fn gc_count() -> u64 {
    GC_COUNT.load(Ordering::Relaxed)
}

/// Where the count of completed collections is kept (native code reads it
/// around a call that may let a collection run: crate users' JITs).
pub fn gc_count_ptr() -> *const u64 {
    GC_COUNT.as_ptr()
}

/// The bytes of the objects the last collection found alive (0 before the
/// first): what the heap's growth goes by, and beside `heap_bytes` how
/// much of the heap is holes.
pub fn live_bytes() -> usize {
    PAGES_LIVE_BYTES.load(Ordering::Relaxed)
}

/// The collections' pauses in nanoseconds, (total, longest, last), since
/// the start or the last reset. With `reset`, the total and the longest
/// start again from here.
pub fn gc_pauses(reset: bool) -> (u64, u64, u64) {
    let last = PAUSE_LAST.load(Ordering::Relaxed);
    if reset {
        (PAUSE_TOTAL.swap(0, Ordering::Relaxed), PAUSE_MAX.swap(0, Ordering::Relaxed), last)
    } else {
        (PAUSE_TOTAL.load(Ordering::Relaxed), PAUSE_MAX.load(Ordering::Relaxed), last)
    }
}

// ---- mutators -------------------------------------------------------------------------

/// Addresses of the cells that point at each live MMTk mutator.
static MUTATORS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

type MmMutator = mmtk::Mutator<Skein>;

/// A thread's allocation context. The MMTk thread handle (`tls`) points at
/// `cell`, which holds the address of the boxed MMTk mutator.
pub struct Mutator {
    mm: Box<MmMutator>,
    cell: Box<AtomicUsize>,
}

const ALLOC_OPTIONS: AllocationOptions = AllocationOptions { allow_overcommit: true, at_safepoint: false, allow_oom_call: true };

impl Default for Mutator {
    fn default() -> Self {
        Self::new()
    }
}

impl Mutator {
    pub fn new() -> Self {
        Self::bound(true)
    }

    /// A mutator; this thread's own (`CURRENT`: its write barrier's) if `mine`.
    fn bound(mine: bool) -> Self {
        init_heap();
        let cell = Box::new(AtomicUsize::new(0));
        let tls = VMMutatorThread(VMThread(OpaquePointer::from_address(Address::from_ptr(&*cell))));
        let mut mm = memory_manager::bind_mutator(mmtk(), tls);
        cell.store(&mut *mm as *mut MmMutator as usize, Ordering::Release);
        MUTATORS.lock().unwrap().push(&*cell as *const AtomicUsize as usize);
        if mine {
            CURRENT.with(|c| c.set(&mut *mm as *mut MmMutator));
        }
        Mutator { mm, cell }
    }

    fn tls(&self) -> VMMutatorThread {
        VMMutatorThread(VMThread(OpaquePointer::from_address(Address::from_ptr(&*self.cell))))
    }

    /// Allocate `words` 8-byte words, 8-byte aligned. Never blocks, never
    /// collects (runtime.md §4.3). Contents are uninitialised.
    #[inline]
    pub fn alloc_words(&mut self, words: usize) -> *mut u64 {
        self.alloc_words_in(words, false)
    }

    /// `alloc_words`, in the space whose objects the collector never moves
    /// if `stays` (M11, plan-m11.md: data may move, the runtime's own
    /// objects don't). A large object doesn't move wherever it's asked for.
    #[inline]
    pub fn alloc_words_in(&mut self, words: usize, stays: bool) -> *mut u64 {
        // (a size from user code must not wrap into a small allocation that
        // the caller then fills past its end)
        let Some(bytes) = words.checked_mul(8).filter(|&b| b <= isize::MAX as usize) else {
            panic!("out of memory allocating {words} words");
        };
        let sem = if bytes > LOS_THRESHOLD.load(Ordering::Relaxed) {
            AllocationSemantics::Los
        } else if stays {
            AllocationSemantics::NonMoving
        } else {
            AllocationSemantics::Default
        };
        let a = memory_manager::alloc_with_options(&mut self.mm, bytes, 8, 0, sem, ALLOC_OPTIONS);
        if a.is_zero() {
            panic!("out of memory allocating {bytes} bytes");
        }
        // SAFETY: MMTk returned a non-null, aligned address.
        let obj = unsafe { ObjectReference::from_raw_address_unchecked(a) };
        memory_manager::post_alloc(&mut self.mm, obj, bytes, sem);
        // (With generations, what's in the non-moving space counts as old
        // from the start: a young collection doesn't look through it. So it
        // must carry the bit that makes the first store into it remember
        // it. MMTk 0.33 sets that for its immortal and large-object spaces
        // and not for an Immix space used as the non-moving one, so it's
        // set here; without it the first young collection freed everything
        // that only a var, a fiber or a proto pointed at.)
        if sem == AllocationSemantics::NonMoving && BARRIER.load(Ordering::Relaxed) {
            <Skein as ObjectModel<Skein>>::GLOBAL_LOG_BIT_SPEC.mark_as_unlogged::<Skein>(obj, Ordering::SeqCst);
        }
        a.to_mut_ptr()
    }
}

impl Drop for Mutator {
    fn drop(&mut self) {
        let mine = &mut *self.mm as *mut MmMutator;
        CURRENT.with(|c| {
            if c.get() == mine {
                c.set(std::ptr::null_mut());
            }
        });
        let me = &*self.cell as *const AtomicUsize as usize;
        MUTATORS.lock().unwrap().retain(|&c| c != me);
        memory_manager::destroy_mutator(&mut self.mm);
    }
}

thread_local! {
    static REQUEST_EPOCH: Cell<u64> = const { Cell::new(0) };
    /// This thread's MMTk mutator, if it made a `Mutator`: where its write
    /// barrier keeps the objects it remembers.
    static CURRENT: Cell<*mut MmMutator> = const { Cell::new(std::ptr::null_mut()) };
}

// ---- the write barrier (M11 part 3) --------------------------------------------------------
//
// A young collection traces only what's new, so it has to be told of every
// old object that has come to point at something new. MMTk's object
// barrier does it: an old object carries a bit (unlogged), and the first
// store into it since the last collection clears the bit and remembers
// the object, whole, for the next young collection to look through.
//
// The check is one bit read, and off altogether unless the plan has
// generations (`BARRIER`). The slow path wants an MMTk mutator to keep
// the list in: this thread's (`CURRENT`), or for a thread that stores into
// the heap without one (the timer thread waking a fiber), one they share
// behind a lock.

/// Is the plan one that needs the barrier?
static BARRIER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SHARED_BARRIER: Mutex<Option<Mutator>> = Mutex::new(None);
/// Objects the barrier has remembered (gc-stats).
static REMEMBERED: AtomicU64 = AtomicU64::new(0);

/// Is the collection now under way a young one? (Only with generations.)
/// It frees nothing old, and looks through the non-moving space not at
/// all, so what's there is alive whatever its mark says.
pub fn in_young_collection() -> bool {
    BARRIER.load(Ordering::Relaxed) && mmtk().get_plan().generational().is_some_and(|g| g.is_current_gc_nursery())
}

/// Is object v in the non-moving space: of a kind that stays put, and not
/// large (a large object is in the large-object space wherever it was
/// asked for, which keeps its own account of what's young)?
pub fn in_nonmoving_space(v: Value) -> bool {
    v.is_obj() && {
        let p = v.ptr() as *const u64;
        kind_at(p).stays_put() && object_words(p) * 8 <= LOS_THRESHOLD.load(Ordering::Relaxed)
    }
}

/// How many objects the write barrier has remembered so far.
pub fn remembered_objects() -> u64 {
    REMEMBERED.load(Ordering::Relaxed)
}

/// The write barrier: `v` is being stored into, or has just been (a field,
/// an atom's value, a stack segment's frames). Call it for every store of
/// a reference into an object that may be old; `Value::set_field` does.
#[inline]
pub fn barrier(v: Value) {
    if BARRIER.load(Ordering::Relaxed) && v.is_obj() {
        barrier_checked(v);
    }
}

#[inline]
fn barrier_checked(v: Value) {
    // SAFETY: an object value's pointer is its MMTk reference.
    let obj = unsafe { ObjectReference::from_raw_address_unchecked(Address::from_mut_ptr(v.ptr())) };
    if <Skein as ObjectModel<Skein>>::GLOBAL_LOG_BIT_SPEC.is_unlogged::<Skein>(obj, Ordering::Relaxed) {
        barrier_slow(obj);
    }
}

#[cold]
fn barrier_slow(obj: ObjectReference) {
    REMEMBERED.fetch_add(1, Ordering::Relaxed);
    let slot = SkeinSlot(obj.to_raw_address());
    let mine = CURRENT.with(|c| c.get());
    if !mine.is_null() {
        // SAFETY: CURRENT is this thread's own live mutator (cleared when it's dropped).
        memory_manager::object_reference_write_post(unsafe { &mut *mine }, obj, slot, None);
    } else {
        let mut shared = SHARED_BARRIER.lock().unwrap();
        let m = shared.get_or_insert_with(|| Mutator::bound(false));
        memory_manager::object_reference_write_post(&mut m.mm, obj, slot, None);
    }
}

/// With generations: has half the budget for what's old been spent since
/// the last full collection? The scheduler asks when its last carrier goes
/// idle, and collects all of the heap then if so (M11 part 5): a full
/// collection stops the program for tens of ms, and one taken while
/// nothing runs is one no request waits for. (Half, not all: the budget
/// spent means a full collection at the next young one, busy or not.)
/// SKEIN_GC_IDLE=0: no collections for being idle (this one, nor the
/// runtime's look for deadlocked fibers when nothing runs).
pub fn idle_collections_off() -> bool {
    static OFF: OnceLock<bool> = OnceLock::new();
    *OFF.get_or_init(|| std::env::var("SKEIN_GC_IDLE").as_deref() == Ok("0"))
}

pub fn idle_collection_due() -> bool {
    if idle_collections_off() {
        return false;
    }
    let (base, limit) = (MATURE_BASE_PAGES.load(Ordering::Relaxed), MATURE_LIMIT_PAGES.load(Ordering::Relaxed));
    if NURSERY_PAGES.load(Ordering::Relaxed) == 0 || limit <= base {
        return false;
    }
    // (what's in use now, less what's been allocated since the last
    // collection, is what was in use after it)
    PAGES_AFTER.load(Ordering::Relaxed).saturating_sub(base) >= (limit - base) / 2
}

/// Collect now, from a safepoint (the caller has saved its stack).
///
/// `whole`: the program asked ((gc)), and means all of the heap. Otherwise
/// (the stress mode) it's a collection like any the heap's growth brings
/// on, which with generations is usually a young one: the kind a missed
/// write barrier shows in.
pub fn collect(m: &mut Mutator, whole: bool) {
    if MMTK_INSTANCE.get().is_none() {
        return;
    }
    REQUEST_EPOCH.with(|e| e.set(crate::safepoint::epoch()));
    #[cfg(feature = "gc-packets")]
    if whole {
        // (see Cargo.toml: MMTk's own start collects all of the heap first)
        static COUNTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !COUNTING.swap(true, Ordering::SeqCst) {
            if let Some(g) = mmtk().get_plan().generational() {
                g.force_full_heap_collection();
            }
            return mmtk().harness_begin(m.tls());
        }
        COUNTING.store(false, Ordering::SeqCst);
        mmtk().harness_end();
    }
    // (with generations, MMTk makes a collection the program asks for a
    // young one unless it's told the next is to be full)
    if whole && let Some(g) = mmtk().get_plan().generational() {
        // (a young collection first, as before the full ones the heap's
        // growth brings on (grow_heap): a full collection marks what's
        // young where it lies, each survivor keeping its block. The
        // overwrite loop was left on 5.9× what's alive after (gc).)
        mmtk().handle_user_collection_request(m.tls(), true, false);
        REQUEST_EPOCH.with(|e| e.set(crate::safepoint::epoch()));
        g.force_full_heap_collection();
    }
    mmtk().handle_user_collection_request(m.tls(), true, whole);
}

// ---- slots: tagged values ----------------------------------------------------------------

/// A heap word holding a tagged `Value`. Only object and cons pointers are
/// references; the tag is preserved on store.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SkeinSlot(pub Address);

impl Slot for SkeinSlot {
    fn load(&self) -> Option<ObjectReference> {
        // SAFETY: slots are addresses of live heap words.
        let w: u64 = unsafe { self.0.load() };
        let tag = w & TAG_MASK;
        if tag == TAG_OBJ || tag == TAG_CONS {
            // SAFETY: a tagged pointer always addresses an MMTk object.
            ObjectReference::from_raw_address(unsafe { Address::from_usize((w & !TAG_MASK) as usize) })
        } else {
            None
        }
    }

    fn store(&self, object: ObjectReference) {
        // SAFETY: as above; non-moving plans never store, but keep it right.
        unsafe {
            let tag = self.0.load::<u64>() & TAG_MASK;
            self.0.store::<u64>(object.to_raw_address().as_usize() as u64 | tag);
        }
    }
}

fn to_ref(v: Value) -> Option<ObjectReference> {
    if v.is_obj() || v.is_cons() { ObjectReference::from_raw_address(Address::from_mut_ptr(v.ptr())) } else { None }
}

// ---- object model -----------------------------------------------------------------------------

#[inline]
pub(crate) fn kind_at(p: *const u64) -> Kind {
    // SAFETY: word 0 of every object is a pointer into LAYOUTS.
    unsafe { (*(*p as *const Layout)).kind }
}

/// Exact size in words of the object at `p` (header included).
pub(crate) fn object_words(p: *const u64) -> usize {
    // SAFETY: p is the start of a live object; payload reads stay inside it.
    size_words(kind_at(p), |i| unsafe { *p.add(2 + i) } as usize)
}

/// Size in words (header included) of an object of `kind` whose payload word
/// `i` is `w(i)`. Shared with the image loader, which sizes objects from file
/// words before they exist in the heap.
pub(crate) fn size_words(kind: Kind, w: impl Fn(usize) -> usize) -> usize {
    match kind {
        Kind::Cons => 3,
        // (a string's length word carries its all-ASCII flag in the top bit)
        Kind::String => 3 + (w(0) & !(1usize << 63)).div_ceil(8),
        Kind::Bytes | Kind::ByteArray => 3 + w(0).div_ceil(8),
        Kind::Float | Kind::Set | Kind::Claim | Kind::Atom => 3,
        Kind::Symbol | Kind::Keyword => 5,
        Kind::Array => 3 + w(0),
        Kind::Vector | Kind::Var | Kind::FnTemplate | Kind::Primitive | Kind::Reader => 6,
        Kind::VecSeq | Kind::Map => 4,
        Kind::MapNode | Kind::CollisionNode => 4 + 2 * w(1),
        Kind::EmptyList => 2,
        Kind::Range => 5,
        Kind::Code => 3 + w(0).div_ceil(2),
        Kind::Proto => 10,
        Kind::Closure => 4 + w(1),
        Kind::Segment => 2 + SEG_HEADER + w(0),
        Kind::Fiber => 2 + fiber::WORDS,
        Kind::Chan => 2 + chan::WORDS,
        Kind::Waiter => 2 + waiter::WORDS,
        Kind::Scope => 2 + scope::WORDS,
        Kind::Class => 2 + class::WORDS,
        Kind::Generic => 2 + generic::WORDS,
        Kind::Instance => {
            5 + (w(2) & !(crate::clos::MUTABLE_INSTANCE as usize)) + usize::from(w(2) & crate::clos::MUTABLE_INSTANCE as usize != 0)
        }
        Kind::Weak => 2 + crate::weak::layout::WORDS,
        Kind::Handle => 2 + crate::conc::handle::WORDS,
        Kind::BigInt => 3 + (w(0) & !(1usize << 63)),
        Kind::Ratio => 4,
        Kind::Stream => 4,
        Kind::Reduced => 3,
        Kind::Sorted => 5,
        Kind::SNode => 7,
        Kind::Queue => 5,
        Kind::Transient => 7,
        Kind::Regex => 3,
    }
}

impl ObjectModel<Skein> for Skein {
    const GLOBAL_LOG_BIT_SPEC: VMGlobalLogBitSpec = VMGlobalLogBitSpec::side_first();
    const LOCAL_FORWARDING_POINTER_SPEC: VMLocalForwardingPointerSpec = VMLocalForwardingPointerSpec::in_header(0);
    const LOCAL_FORWARDING_BITS_SPEC: VMLocalForwardingBitsSpec = VMLocalForwardingBitsSpec::side_first();
    const LOCAL_MARK_BIT_SPEC: VMLocalMarkBitSpec = VMLocalMarkBitSpec::side_after(Self::LOCAL_FORWARDING_BITS_SPEC.as_spec());
    const LOCAL_LOS_MARK_NURSERY_SPEC: VMLocalLOSMarkNurserySpec =
        VMLocalLOSMarkNurserySpec::side_after(Self::LOCAL_MARK_BIT_SPEC.as_spec());
    const OBJECT_REF_OFFSET_LOWER_BOUND: isize = 0;

    /// Move an object (M11): its words as they are, header and all, to
    /// where the collector makes room. The identity hash is in the header
    /// and goes with it. MMTk then writes where it went over the old
    /// copy's first word (the forwarding pointer), so whoever still held
    /// the old address would read that as a layout: no reference may
    /// outlive a collection but through a slot the collector rewrites.
    fn copy(from: ObjectReference, semantics: CopySemantics, context: &mut GCWorkerCopyContext<Skein>) -> ObjectReference {
        let bytes = Self::get_current_size(from);
        let to = context.alloc_copy(from, bytes, 8, 0, semantics);
        assert!(!to.is_zero(), "the collector found no room to move {bytes} bytes to");
        // SAFETY: `to` is fresh room for `bytes` bytes, and `from` an object of that size.
        let moved = unsafe {
            std::ptr::copy_nonoverlapping::<u8>(from.to_raw_address().to_ptr(), to.to_mut_ptr(), bytes);
            ObjectReference::from_raw_address_unchecked(to)
        };
        context.post_copy(moved, bytes, semantics);
        MOVED.fetch_add(1, Ordering::Relaxed);
        moved
    }
    fn copy_to(_: ObjectReference, _: ObjectReference, _: Address) -> Address {
        unreachable!("only a compacting plan copies in place, and Skein has none")
    }
    fn get_reference_when_copied_to(_: ObjectReference, to: Address) -> ObjectReference {
        // SAFETY: an object's reference is its first word's address.
        unsafe { ObjectReference::from_raw_address_unchecked(to) }
    }
    fn get_current_size(object: ObjectReference) -> usize {
        object_words(object.to_raw_address().to_ptr()) * 8
    }
    fn get_size_when_copied(object: ObjectReference) -> usize {
        Self::get_current_size(object)
    }
    fn get_align_when_copied(_: ObjectReference) -> usize {
        8
    }
    fn get_align_offset_when_copied(_: ObjectReference) -> usize {
        0
    }
    fn get_type_descriptor(_: ObjectReference) -> &'static [i8] {
        &[]
    }
    fn ref_to_object_start(object: ObjectReference) -> Address {
        object.to_raw_address()
    }
    fn ref_to_header(object: ObjectReference) -> Address {
        object.to_raw_address()
    }
    fn dump_object(object: ObjectReference) {
        eprintln!("{:?}: {:?}", object, kind_at(object.to_raw_address().to_ptr()));
    }
}

// ---- scanning ------------------------------------------------------------------------------------

fn slot_at(p: *const u64) -> SkeinSlot {
    SkeinSlot(Address::from_ptr(p))
}

/// Visit every value-holding word of the object at `p`.
pub(crate) fn scan(p: *mut u64, visit: &mut dyn FnMut(SkeinSlot)) {
    // SAFETY: offsets below are within the object's payload (see object_words).
    let field = |i: usize| unsafe { p.add(2 + i) as *const u64 };
    let w = |i: usize| unsafe { *p.add(2 + i) } as usize;
    let fields = |idx: &[usize], visit: &mut dyn FnMut(SkeinSlot)| {
        for &i in idx {
            visit(slot_at(field(i)));
        }
    };
    match kind_at(p) {
        Kind::Cons => {
            // SAFETY: conses are [layout][car][cdr].
            unsafe {
                visit(slot_at(p.add(1)));
                visit(slot_at(p.add(2)));
            }
        }
        Kind::String
        | Kind::Bytes
        | Kind::ByteArray
        | Kind::Float
        | Kind::Range
        | Kind::Code
        | Kind::EmptyList
        | Kind::Claim
        | Kind::BigInt => {}
        Kind::Ratio => fields(&[0, 1], visit),
        Kind::Stream => fields(&[1], visit),
        Kind::Reduced | Kind::Regex => fields(&[0], visit),
        Kind::Sorted => fields(&[crate::colls::SORTED_ROOT], visit),
        Kind::SNode => fields(&[0, 1, 2, 3], visit),
        Kind::Queue => fields(&[crate::colls::Q_FRONT, crate::colls::Q_REAR], visit),
        Kind::Transient => fields(&[crate::colls::T_OWNER, crate::colls::T_DATA], visit),
        Kind::Symbol | Kind::Keyword => fields(&[0, 1], visit),
        Kind::Array => (1..=w(0)).for_each(|i| visit(slot_at(field(i)))),
        Kind::Vector => fields(&[2, 3], visit),
        Kind::VecSeq | Kind::Set | Kind::Atom | Kind::Primitive => fields(&[0], visit),
        Kind::Map => fields(&[1], visit),
        Kind::MapNode | Kind::CollisionNode => (2..2 + 2 * w(1)).for_each(|i| visit(slot_at(field(i)))),
        Kind::Var => fields(&[0, 1, 2], visit),
        Kind::Proto => fields(&[0, 4, 5, 6], visit),
        Kind::FnTemplate => fields(&[0, 1, 3], visit),
        Kind::Closure => {
            visit(slot_at(field(0)));
            (2..2 + w(1)).for_each(|i| visit(slot_at(field(i))));
        }
        // Frames are scanned through the owning fiber, which knows how much
        // of each segment is live; the segment itself only links onward.
        Kind::Segment => fields(&[1], visit),
        Kind::Fiber => {
            fields(fiber::VALUE_FIELDS, visit);
            // SAFETY: SEG is a segment or nil.
            let mut seg = Value(unsafe { *field(fiber::SEG) });
            while seg.is(Kind::Segment) {
                let data = seg.seg_data() as *const u64;
                for i in 0..seg.seg_live_words() {
                    // SAFETY: i is below the segment's live top.
                    visit(slot_at(unsafe { data.add(i) }));
                }
                seg = seg.seg_prev();
            }
        }
        Kind::Chan => fields(chan::VALUE_FIELDS, visit),
        Kind::Waiter => fields(&[0, 1, 2, 3], visit),
        Kind::Scope => fields(scope::VALUE_FIELDS, visit),
        Kind::Reader => fields(&[0, 2], visit),
        Kind::Class => fields(class::VALUE_FIELDS, visit),
        Kind::Generic => fields(generic::VALUE_FIELDS, visit),
        Kind::Instance => {
            fields(&[0, 1], visit);
            // The slots, then a mutable instance's owner word.
            let n = (w(2) as u64 & !crate::clos::MUTABLE_INSTANCE) as usize;
            let owner = usize::from(w(2) as u64 & crate::clos::MUTABLE_INSTANCE != 0);
            (3..3 + n + owner).for_each(|i| visit(slot_at(field(i))));
        }
        // The referent is weak: only the cleanup is traced.
        Kind::Weak => fields(&[crate::weak::layout::CLEANUP], visit),
        Kind::Handle => fields(crate::conc::handle::VALUE_FIELDS, visit),
    }
}

type RootFn = fn(&mut dyn FnMut(Value));
static EXTRA_ROOTS: OnceLock<RootFn> = OnceLock::new();

/// Register the VM's out-of-heap roots (run queues, timers, running fibers).
pub fn set_extra_roots(f: RootFn) {
    let _ = EXTRA_ROOTS.set(f);
}

/// Visit the VM's out-of-heap roots.
pub fn for_each_extra_root(f: &mut dyn FnMut(Value)) {
    if let Some(r) = EXTRA_ROOTS.get() {
        r(f);
    }
}

impl Scanning<Skein> for Skein {
    fn scan_object<SV: SlotVisitor<SkeinSlot>>(_: VMWorkerThread, object: ObjectReference, sv: &mut SV) {
        let p: *mut u64 = object.to_raw_address().to_mut_ptr();
        #[cfg(feature = "gc-check")]
        scan(p, &mut |s| {
            check_slot(p, s);
            sv.visit_slot(s)
        });
        #[cfg(not(feature = "gc-check"))]
        scan(p, &mut |s| sv.visit_slot(s));
    }
    fn notify_initial_thread_scan_complete(_: bool, _: VMWorkerThread) {}
    fn scan_roots_in_mutator_thread(_: VMWorkerThread, _: &'static mut MmMutator, _: impl RootsWorkFactory<SkeinSlot>) {
        // Carriers have no thread-local roots: a fiber's stack lives in the
        // heap, and the running fibers are reported as VM roots.
    }
    fn scan_vm_specific_roots(_: VMWorkerThread, mut factory: impl RootsWorkFactory<SkeinSlot>) {
        let began = std::time::Instant::now();
        let mut nodes = Vec::new();
        let mut add = |v: Value| {
            if let Some(r) = to_ref(v) {
                #[cfg(feature = "gc-check")]
                if mmtk::memory_manager::is_mmtk_object(r.to_raw_address()).is_none() {
                    panic!("gc-check: a root is no object: {:#x}", v.0);
                }
                nodes.push(r);
            }
        };
        crate::runtime::for_each_root(&mut add);
        crate::image::for_each_root(&mut add);
        if let Some(f) = EXTRA_ROOTS.get() {
            f(&mut add);
        }
        PHASE_ROOT_COUNT.store(nodes.len(), Ordering::Relaxed);
        factory.create_process_pinning_roots_work(nodes);
        PHASE_ROOTS.store(began.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    fn process_weak_refs(worker: &mut mmtk::scheduler::GCWorker<Skein>, tracer: impl mmtk::vm::ObjectTracerContext<Skein>) -> bool {
        let began = std::time::Instant::now();
        // Parked fibers nothing reached are kept, deadlocked (deadlock.rs):
        // first, so that what they hold is reached before weak references
        // to it are cleared. Keeping any runs the trace again, then this.
        if crate::deadlock::process(worker, &tracer) {
            return true;
        }
        crate::weak::process();
        PHASE_WEAK.store(began.elapsed().as_nanos() as u64, Ordering::Relaxed);
        false
    }
    fn supports_return_barrier() -> bool {
        false
    }
    fn prepare_for_roots_re_scanning() {}
}

/// gc-check, after every collection (the world still stopped): everything
/// reachable from the roots, looked through as the collector would, must
/// hold only references to live objects. This is M11 part 3's check on the
/// write barrier: a young collection traces only what it was told of, so
/// an old object that came to point at a new one with no barrier is left
/// holding the address of something freed or moved. A full collection
/// never shows that; this walk does, at the first collection after it,
/// and says which kind of object and which word.
#[cfg(feature = "gc-check")]
fn verify_heap() {
    let mut seen = std::collections::HashSet::new();
    let mut todo: Vec<*mut u64> = Vec::new();
    let mut root = |v: Value| {
        if let Some(r) = to_ref(v) {
            if mmtk::memory_manager::is_mmtk_object(r.to_raw_address()).is_none() {
                panic!("gc-check: after a collection, a root is no object: {:#x}", v.0);
            }
            let p: *mut u64 = r.to_raw_address().to_mut_ptr();
            if seen.insert(p as usize) {
                todo.push(p);
            }
        }
    };
    crate::runtime::for_each_root(&mut root);
    crate::image::for_each_root(&mut root);
    if let Some(f) = EXTRA_ROOTS.get() {
        f(&mut root);
    }
    while let Some(p) = todo.pop() {
        scan(p, &mut |s| {
            if let Some(r) = s.load() {
                if mmtk::memory_manager::is_mmtk_object(r.to_raw_address()).is_none() {
                    // SAFETY: the slot is within the object being looked through.
                    let w: u64 = unsafe { s.0.load() };
                    let index = (s.0.as_usize() as isize - p as isize) / 8;
                    // SAFETY: p is a live object's address.
                    let holder = unsafe { ObjectReference::from_raw_address_unchecked(Address::from_mut_ptr(p)) };
                    let unlogged = <Skein as ObjectModel<Skein>>::GLOBAL_LOG_BIT_SPEC.is_unlogged::<Skein>(holder, Ordering::Relaxed);
                    panic!(
                        "gc-check: after collection {}, word {index} of a {:?} at {p:p} is {w:#x}, which is no object (the holder is {}): a store with no write barrier?",
                        GC_COUNT.load(Ordering::Relaxed),
                        kind_at(p),
                        if unlogged { "old, and not remembered" } else { "new, or remembered" }
                    );
                }
                let q: *mut u64 = r.to_raw_address().to_mut_ptr();
                if seen.insert(q as usize) {
                    todo.push(q);
                }
            }
        });
    }
}

/// gc-check: the word in slot s of the object at p must be a live object
/// if it's a pointer; else panic, saying where it was.
#[cfg(feature = "gc-check")]
fn check_slot(p: *mut u64, s: SkeinSlot) {
    if let Some(r) = s.load()
        && mmtk::memory_manager::is_mmtk_object(r.to_raw_address()).is_none()
    {
        // SAFETY: the slot is within the object being scanned.
        let w: u64 = unsafe { s.0.load() };
        let index = (s.0.as_usize() - p as usize) / 8;
        let kind = kind_at(p);
        let extra = if kind == Kind::Fiber { String::new() } else { String::new() };
        panic!("gc-check: word {index} of a {kind:?} at {p:p} is {w:#x}, which is no object{extra}");
    }
}

// ---- collection: the safepoint protocol -------------------------------------------------------

impl Collection<Skein> for Skein {
    fn stop_all_mutators<F: FnMut(&'static mut MmMutator)>(_: VMWorkerThread, mut visitor: F) {
        *PAUSE_BEGAN.lock().unwrap() = Some(std::time::Instant::now());
        PAGES_BEFORE.store(mmtk().get_plan().get_reserved_pages(), Ordering::Relaxed);
        let began = std::time::Instant::now();
        crate::safepoint::stop_world();
        PHASE_STOP.store(began.elapsed().as_nanos() as u64, Ordering::Relaxed);
        for m in Skein::mutators() {
            visitor(m);
        }
    }
    fn create_gc_trigger() -> Box<dyn GCTriggerPolicy<Skein>> {
        let min = HEAP_MIN_PAGES.load(Ordering::Relaxed);
        HEAP_LIMIT_PAGES.store(min, Ordering::Relaxed);
        Box::new(ProportionalTrigger)
    }
    fn resume_mutators(_: VMWorkerThread) {
        GC_COUNT.fetch_add(1, Ordering::Relaxed);
        if HEAP_MAX_PAGES.load(Ordering::Relaxed) != 0 {
            grow_heap(mmtk());
        }
        #[cfg(feature = "gc-check")]
        verify_heap();
        if let Some(began) = PAUSE_BEGAN.lock().unwrap().take() {
            let ns = began.elapsed().as_nanos() as u64;
            PAUSE_TOTAL.fetch_add(ns, Ordering::Relaxed);
            PAUSE_MAX.fetch_max(ns, Ordering::Relaxed);
            PAUSE_LAST.store(ns, Ordering::Relaxed);
            if GC_LOG.load(Ordering::Relaxed) != 0 {
                let mb = |pages: &AtomicUsize| (pages.load(Ordering::Relaxed) << LOG_BYTES_IN_PAGE) as f64 / 1048576.0;
                eprintln!(
                    "gc {}{}: {:.1} ms, {:.0} MB -> {:.0} MB ({:.0} MB live), next at {:.0} MB",
                    GC_COUNT.load(Ordering::Relaxed),
                    // (with generations: was it a young collection or a full one?)
                    match mmtk().get_plan().generational() {
                        Some(g) if g.is_current_gc_nursery() => " young",
                        Some(_) => " full",
                        None => "",
                    },
                    ns as f64 / 1e6,
                    mb(&PAGES_BEFORE),
                    mb(&PAGES_AFTER),
                    mb(&PAGES_LIVE),
                    mb(&PAGES_LIMIT)
                );
                if GC_LOG.load(Ordering::Relaxed) > 1 {
                    let ms = |ns: &AtomicU64| ns.load(Ordering::Relaxed) as f64 / 1e6;
                    eprintln!(
                        "  stopping {:.2} ms, {} roots {:.2} ms, weak references {:.2} ms",
                        ms(&PHASE_STOP),
                        PHASE_ROOT_COUNT.load(Ordering::Relaxed),
                        ms(&PHASE_ROOTS),
                        ms(&PHASE_WEAK)
                    );
                }
            }
        }
        crate::safepoint::resume_world();
    }
    fn block_for_gc(_: VMMutatorThread) {
        let since = REQUEST_EPOCH.with(|e| e.get());
        crate::safepoint::wait_for_pause_after(since);
    }
    fn spawn_gc_thread(_: VMThread, ctx: GCThreadContext<Skein>) {
        let GCThreadContext::Worker(worker) = ctx;
        std::thread::Builder::new()
            .name("skein-gc".into())
            // (a worker that moves objects takes memory for them, and MMTk
            // maps it with a table on its stack: Rust's 2 MB overflowed.
            // Only the pages touched are committed.)
            .stack_size(16 << 20)
            .spawn(move || {
                // MMTk wants a distinct, non-null handle per worker. Ours
                // carry nothing: the address of a per-thread byte will do.
                // (`is_mutator` never matches it: only mutator cells count.)
                let me: &'static u8 = Box::leak(Box::new(0));
                let tls = VMWorkerThread(VMThread(OpaquePointer::from_address(Address::from_ptr(me))));
                memory_manager::start_worker(mmtk(), tls, worker)
            })
            .expect("failed to start GC worker");
    }
}

impl ActivePlan<Skein> for Skein {
    fn is_mutator(tls: VMThread) -> bool {
        let a = tls.0.to_address().as_usize();
        a != 0 && MUTATORS.lock().unwrap().contains(&a)
    }
    fn mutator(tls: VMMutatorThread) -> &'static mut MmMutator {
        // SAFETY: tls points at a registered cell holding a live mutator.
        unsafe {
            let cell = &*(tls.0.0.to_address().to_ptr::<AtomicUsize>());
            &mut *(cell.load(Ordering::Acquire) as *mut MmMutator)
        }
    }
    fn mutators<'a>() -> Box<dyn Iterator<Item = &'a mut MmMutator> + 'a> {
        let cells = MUTATORS.lock().unwrap().clone();
        // SAFETY: as in `mutator`; called with the world stopped.
        Box::new(cells.into_iter().map(|c| unsafe { &mut *((*(c as *const AtomicUsize)).load(Ordering::Acquire) as *mut MmMutator) }))
    }
    fn number_of_mutators() -> usize {
        MUTATORS.lock().unwrap().len()
    }
}

// ---- no weak references yet ---------------------------------------------------------------------

#[derive(Debug)]
pub struct NoFinalizable(ObjectReference);

impl Finalizable for NoFinalizable {
    fn get_reference(&self) -> ObjectReference {
        self.0
    }
    fn set_reference(&mut self, object: ObjectReference) {
        self.0 = object;
    }
    fn keep_alive<OT: mmtk::vm::ObjectTracer>(&mut self, _: &mut OT) {}
}

impl ReferenceGlue<Skein> for Skein {
    type FinalizableType = NoFinalizable;
    fn clear_referent(_: ObjectReference) {}
    fn get_referent(_: ObjectReference) -> Option<ObjectReference> {
        None
    }
    fn set_referent(_: ObjectReference, _: ObjectReference) {}
    fn enqueue_references(_: &[ObjectReference], _: VMWorkerThread) {}
}
