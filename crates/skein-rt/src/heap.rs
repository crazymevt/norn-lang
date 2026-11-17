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
use mmtk::util::alloc::AllocationOptions;
use mmtk::util::copy::{CopySemantics, GCWorkerCopyContext};
use mmtk::util::opaque_pointer::{OpaquePointer, VMMutatorThread, VMThread, VMWorkerThread};
use mmtk::Plan;
use mmtk::util::constants::LOG_BYTES_IN_PAGE;
use mmtk::util::heap::{GCTriggerPolicy, SpaceStats};
use mmtk::util::options::{GCTriggerSelector, PlanSelector};
use mmtk::util::{Address, ObjectReference};
use mmtk::vm::slot::{Slot, UnimplementedMemorySlice};
use mmtk::vm::{
    ActivePlan, Collection, Finalizable, GCThreadContext, ObjectModel, ReferenceGlue, RootsWorkFactory, Scanning,
    SlotVisitor, VMBinding, VMGlobalLogBitSpec, VMLocalForwardingBitsSpec, VMLocalForwardingPointerSpec,
    VMLocalLOSMarkNurserySpec, VMLocalMarkBitSpec,
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
// After each collection the heap may grow to what's live times (1 +
// SKEIN_HEAP_GROWTH percent), 100 by default: a collection when the heap has
// doubled, as Go's GOGC=100; within SKEIN_HEAP_MIN (64 MB) and SKEIN_HEAP_MAX
// (8 GB). MMTk's own dynamic size (MemBalancer, SKEIN_HEAP_GROWTH=balancer)
// sizes the room above live data by how fast collections trace against how
// fast the program allocates; Skein traces fast and allocates a lot, so it
// left a few MB above live data, and a program with 65 MB live collected
// every few MB it allocated: linting a 34 KB file took about 100
// collections and 550 ms, against 12 or so and 150 ms now.

static HEAP_MIN_PAGES: AtomicUsize = AtomicUsize::new(0);
static HEAP_MAX_PAGES: AtomicUsize = AtomicUsize::new(0);
static HEAP_GROWTH: AtomicUsize = AtomicUsize::new(100);

struct ProportionalTrigger {
    /// The heap's size now, in pages: a collection when it's full.
    current: AtomicUsize,
    /// Pages that allocations waiting on this collection need.
    pending: AtomicUsize,
}

impl GCTriggerPolicy<Skein> for ProportionalTrigger {
    fn on_pending_allocation(&self, pages: usize) {
        self.pending.fetch_add(pages, Ordering::SeqCst);
    }
    fn on_gc_end(&self, mmtk: &'static MMTK<Skein>) {
        // (what's live, with the plan's reserve for collecting)
        let live = mmtk.get_plan().get_reserved_pages();
        let growth = HEAP_GROWTH.load(Ordering::Relaxed);
        let want = live + live / 100 * growth + self.pending.swap(0, Ordering::SeqCst);
        let (min, max) = (HEAP_MIN_PAGES.load(Ordering::Relaxed), HEAP_MAX_PAGES.load(Ordering::Relaxed));
        self.current.store(want.clamp(min, max), Ordering::Relaxed);
    }
    fn is_gc_required(&self, space_full: bool, space: Option<SpaceStats<Skein>>, plan: &dyn Plan<VM = Skein>) -> bool {
        plan.collection_required(space_full, space)
    }
    fn is_heap_full(&self, plan: &dyn Plan<VM = Skein>) -> bool {
        plan.get_reserved_pages() > self.current.load(Ordering::Relaxed)
    }
    fn get_current_heap_size_in_pages(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }
    fn get_max_heap_size_in_pages(&self) -> usize {
        HEAP_MAX_PAGES.load(Ordering::Relaxed)
    }
    fn can_heap_size_grow(&self) -> bool {
        self.current.load(Ordering::Relaxed) < HEAP_MAX_PAGES.load(Ordering::Relaxed)
    }
}

/// Initialise MMTk (once per process). Collection stays disabled until
/// `enable_collection`, so boot code needs no safepoints.
pub fn init_heap() {
    let m = *MMTK_INSTANCE.get_or_init(|| {
        let mut b = MMTKBuilder::new();
        b.options.plan.set(PlanSelector::Immix);
        let max = env_bytes("SKEIN_HEAP_MAX").unwrap_or(8 << 30);
        let min = env_bytes("SKEIN_HEAP_MIN").unwrap_or(64 << 20).min(max);
        let growth = std::env::var("SKEIN_HEAP_GROWTH").ok();
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

/// Number of completed collections.
pub fn gc_count() -> u64 {
    GC_COUNT.load(Ordering::Relaxed)
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

const ALLOC_OPTIONS: AllocationOptions =
    AllocationOptions { allow_overcommit: true, at_safepoint: false, allow_oom_call: true };

impl Default for Mutator {
    fn default() -> Self {
        Self::new()
    }
}

impl Mutator {
    pub fn new() -> Self {
        init_heap();
        let cell = Box::new(AtomicUsize::new(0));
        let tls = VMMutatorThread(VMThread(OpaquePointer::from_address(Address::from_ptr(&*cell))));
        let mut mm = memory_manager::bind_mutator(mmtk(), tls);
        cell.store(&mut *mm as *mut MmMutator as usize, Ordering::Release);
        MUTATORS.lock().unwrap().push(&*cell as *const AtomicUsize as usize);
        Mutator { mm, cell }
    }

    fn tls(&self) -> VMMutatorThread {
        VMMutatorThread(VMThread(OpaquePointer::from_address(Address::from_ptr(&*self.cell))))
    }

    /// Allocate `words` 8-byte words, 8-byte aligned. Never blocks, never
    /// collects (runtime.md §4.3). Contents are uninitialised.
    #[inline]
    pub fn alloc_words(&mut self, words: usize) -> *mut u64 {
        let bytes = words * 8;
        let sem = if bytes > LOS_THRESHOLD.load(Ordering::Relaxed) {
            AllocationSemantics::Los
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
        a.to_mut_ptr()
    }
}

impl Drop for Mutator {
    fn drop(&mut self) {
        let me = &*self.cell as *const AtomicUsize as usize;
        MUTATORS.lock().unwrap().retain(|&c| c != me);
        memory_manager::destroy_mutator(&mut self.mm);
    }
}

thread_local! {
    static REQUEST_EPOCH: Cell<u64> = const { Cell::new(0) };
}

/// Collect now, from a safepoint (the caller has saved its stack).
pub fn collect(m: &mut Mutator) {
    if MMTK_INSTANCE.get().is_none() {
        return;
    }
    REQUEST_EPOCH.with(|e| e.set(crate::safepoint::epoch()));
    memory_manager::handle_user_collection_request(mmtk(), m.tls());
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
        Kind::Instance => 5 + (w(2) & !(crate::clos::MUTABLE_INSTANCE as usize)) + usize::from(w(2) & crate::clos::MUTABLE_INSTANCE as usize != 0),
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
    const LOCAL_MARK_BIT_SPEC: VMLocalMarkBitSpec =
        VMLocalMarkBitSpec::side_after(Self::LOCAL_FORWARDING_BITS_SPEC.as_spec());
    const LOCAL_LOS_MARK_NURSERY_SPEC: VMLocalLOSMarkNurserySpec =
        VMLocalLOSMarkNurserySpec::side_after(Self::LOCAL_MARK_BIT_SPEC.as_spec());
    const OBJECT_REF_OFFSET_LOWER_BOUND: isize = 0;

    fn copy(_: ObjectReference, _: CopySemantics, _: &mut GCWorkerCopyContext<Skein>) -> ObjectReference {
        unreachable!("Skein uses a non-moving plan")
    }
    fn copy_to(_: ObjectReference, _: ObjectReference, _: Address) -> Address {
        unreachable!("Skein uses a non-moving plan")
    }
    fn get_reference_when_copied_to(_: ObjectReference, _: Address) -> ObjectReference {
        unreachable!("Skein uses a non-moving plan")
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
        Kind::String | Kind::Bytes | Kind::ByteArray | Kind::Float | Kind::Range | Kind::Code | Kind::EmptyList | Kind::Claim | Kind::BigInt => {}
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
        factory.create_process_pinning_roots_work(nodes);
    }
    fn process_weak_refs(
        _: &mut mmtk::scheduler::GCWorker<Skein>,
        _: impl mmtk::vm::ObjectTracerContext<Skein>,
    ) -> bool {
        crate::weak::process();
        false
    }
    fn supports_return_barrier() -> bool {
        false
    }
    fn prepare_for_roots_re_scanning() {}
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
        let extra = if kind == Kind::Fiber {
            String::new()
        } else {
            String::new()
        };
        panic!("gc-check: word {index} of a {kind:?} at {p:p} is {w:#x}, which is no object{extra}");
    }
}

// ---- collection: the safepoint protocol -------------------------------------------------------

impl Collection<Skein> for Skein {
    fn stop_all_mutators<F: FnMut(&'static mut MmMutator)>(_: VMWorkerThread, mut visitor: F) {
        crate::safepoint::stop_world();
        for m in Skein::mutators() {
            visitor(m);
        }
    }
    fn create_gc_trigger() -> Box<dyn GCTriggerPolicy<Skein>> {
        let min = HEAP_MIN_PAGES.load(Ordering::Relaxed);
        Box::new(ProportionalTrigger { current: AtomicUsize::new(min), pending: AtomicUsize::new(0) })
    }
    fn resume_mutators(_: VMWorkerThread) {
        GC_COUNT.fetch_add(1, Ordering::Relaxed);
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
        Box::new(
            cells.into_iter().map(|c| unsafe { &mut *((*(c as *const AtomicUsize)).load(Ordering::Acquire) as *mut MmMutator) }),
        )
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
