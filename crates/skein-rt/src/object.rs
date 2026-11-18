//! Heap object model. See docs/internals/runtime.md §2.2.
//!
//! ```text
//!  word 0   layout*   → Layout (kind, name; class version from M2)
//!  word 1   meta      identity hash (lazily assigned)
//!  word 2…  payload
//! ```
//!
//! Conses have a one-word header (layout only) so the GC can find every
//! object's size from the object itself: `[layout][car][cdr]`. Their tag
//! still identifies them without touching memory.
//!
//! Accessors here are `unsafe` in spirit: they assume the value really is the
//! kind of object the caller says it is. Callers check `is(kind)` first.

use crate::heap::Mutator;
use crate::value::{TAG_CONS, TAG_OBJ, Value};
use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! kinds {
    ($($k:ident => $name:literal),* $(,)?) => {
        #[repr(u8)]
        #[derive(Copy, Clone, PartialEq, Eq, Debug)]
        pub enum Kind { $($k),* }
        pub static LAYOUTS: &[Layout] = &[$(Layout { kind: Kind::$k, name: $name }),*];
    };
}

kinds! {
    String => "String",
    Symbol => "Symbol",
    Keyword => "Keyword",
    Float => "Float",
    Array => "Array",
    Vector => "Vector",
    VecSeq => "VecSeq",
    Map => "Map",
    MapNode => "MapNode",
    CollisionNode => "CollisionNode",
    Set => "Set",
    EmptyList => "EmptyList",
    Range => "Range",
    Var => "Var",
    Code => "Code",
    Proto => "Proto",
    FnTemplate => "FnTemplate",
    Closure => "Closure",
    Primitive => "Primitive",
    Segment => "StackSegment",
    Cons => "Cons",
    Fiber => "Fiber",
    Chan => "Channel",
    Waiter => "Waiter",
    Claim => "Claim",
    Scope => "Scope",
    Atom => "Atom",
    Reader => "Reader",
    Class => "Class",
    Generic => "GenericFunction",
    Instance => "Instance",
    Weak => "WeakRef",
    Bytes => "Bytes",
    Handle => "Handle",
    BigInt => "BigInt",
    Ratio => "Ratio",
    Stream => "Stream",
    Reduced => "Reduced",
    Sorted => "Sorted",
    SNode => "SNode",
    Queue => "Queue",
    Transient => "Transient",
    Regex => "Regex",
    // (new kinds go last: images record kinds by their place in this list)
    ByteArray => "ByteArray",
}

/// Static per-kind descriptor. From M2 layouts become heap objects that also
/// identify a class *version* (runtime.md §2.2).
pub struct Layout {
    pub kind: Kind,
    pub name: &'static str,
}

pub const HEADER_WORDS: usize = 2;

static NEXT_IDENTITY_HASH: AtomicU64 = AtomicU64::new(0x9e37_79b9);

/// The identity-hash counter, saved in images so that hashes assigned after
/// a restore don't repeat ones already stored in object headers.
pub fn identity_hash_counter() -> u64 {
    NEXT_IDENTITY_HASH.load(Ordering::Relaxed)
}

pub fn set_identity_hash_counter(n: u64) {
    NEXT_IDENTITY_HASH.fetch_max(n, Ordering::Relaxed);
}

impl Kind {
    /// Is an object of this kind one the collector must leave where it is?
    /// (M11, plan-m11.md: *data moves, the runtime's own objects don't*.)
    ///
    /// These are what the interpreter and compiled code hold addresses of
    /// (stack segments, code, protos; classes, generic functions and
    /// keywords, which tier 1 writes into its code), what Rust keeps by
    /// address in tables of its own (symbols, vars, handles, weak
    /// references), and the objects fibers meet at (fibers, channels and
    /// their waiters, scopes, atoms). They're few beside a program's data,
    /// and mostly long-lived. Everything else may be moved: strings,
    /// numbers, collections and their nodes, instances, closures.
    ///
    /// An `Array` is data, except a proto's constants, which the
    /// interpreter reads through a pointer (`alloc_staying`).
    pub fn stays_put(self) -> bool {
        matches!(
            self,
            Kind::Symbol
                | Kind::Keyword
                | Kind::Var
                | Kind::Code
                | Kind::Proto
                | Kind::FnTemplate
                | Kind::Primitive
                | Kind::Segment
                | Kind::Fiber
                | Kind::Chan
                | Kind::Waiter
                | Kind::Claim
                | Kind::Scope
                | Kind::Atom
                | Kind::Reader
                | Kind::Class
                | Kind::Generic
                | Kind::Weak
                | Kind::Handle
                | Kind::Regex
        )
    }
}

/// Allocate an object with `payload_words` words after the header, where
/// its kind belongs (`Kind::stays_put`).
#[inline]
pub fn alloc(m: &mut Mutator, kind: Kind, payload_words: usize) -> Value {
    alloc_in(m, kind, payload_words, kind.stays_put())
}

/// `alloc`, for an object that must stay where it is though its kind is
/// one that may move: an array the interpreter reads through a pointer.
#[inline]
pub fn alloc_staying(m: &mut Mutator, kind: Kind, payload_words: usize) -> Value {
    alloc_in(m, kind, payload_words, true)
}

#[inline]
fn alloc_in(m: &mut Mutator, kind: Kind, payload_words: usize, stays: bool) -> Value {
    let p = m.alloc_words_in(HEADER_WORDS + payload_words, stays);
    // SAFETY: p points to at least HEADER_WORDS fresh words.
    unsafe {
        p.write(&LAYOUTS[kind as usize] as *const Layout as u64);
        p.add(1).write(0);
    }
    Value(p as u64 | TAG_OBJ)
}

pub const CONS_WORDS: usize = 3;

/// Allocate a cons cell.
#[inline]
pub fn cons(m: &mut Mutator, car: Value, cdr: Value) -> Value {
    let p = m.alloc_words(CONS_WORDS);
    // SAFETY: three fresh words.
    unsafe {
        p.write(&LAYOUTS[Kind::Cons as usize] as *const Layout as u64);
        p.add(1).write(car.0);
        p.add(2).write(cdr.0);
    }
    Value(p as u64 | TAG_CONS)
}

impl Value {
    #[inline]
    pub fn layout(self) -> &'static Layout {
        debug_assert!(self.is_obj() || self.is_cons());
        // SAFETY: every object's word 0 is a pointer into LAYOUTS.
        unsafe { &*(*self.ptr() as *const Layout) }
    }

    #[inline]
    pub fn obj_kind(self) -> Kind {
        self.layout().kind
    }

    /// Is this a heap object of kind `k`?
    #[inline]
    pub fn is(self, k: Kind) -> bool {
        self.is_obj() && self.obj_kind() == k
    }

    #[inline]
    pub fn payload_ptr(self) -> *mut u64 {
        // SAFETY: objects always have a header.
        unsafe { self.ptr().add(HEADER_WORDS) }
    }

    #[inline]
    pub fn word(self, i: usize) -> u64 {
        // SAFETY: caller guarantees i is within the payload.
        unsafe { *self.payload_ptr().add(i) }
    }

    #[inline]
    pub fn set_word(self, i: usize, w: u64) {
        // SAFETY: caller guarantees i is within the payload.
        unsafe { *self.payload_ptr().add(i) = w }
    }

    #[inline]
    pub fn field(self, i: usize) -> Value {
        Value(self.word(i))
    }

    /// Store value v in payload word i. Every store of a reference into
    /// an object goes through here or says so itself (`heap::barrier`):
    /// a young collection has to hear of an old object that comes to
    /// point at a new one (M11).
    #[inline]
    pub fn set_field(self, i: usize, v: Value) {
        self.set_word(i, v.0);
        if v.is_obj() || v.is_cons() {
            crate::heap::barrier(self);
        }
    }

    /// Store a tagged value into payload word `i` of an object made just
    /// now: no write barrier. Only for data (a kind that isn't
    /// `stays_put`: those count as old from birth), and only before the
    /// next safepoint, which within a primitive is always: a new object
    /// is young, and a young collection looks through every young object
    /// it reaches. (set_field's check, 40 times over for each node a map
    /// copies, made assoc a fifth slower with generations.)
    #[inline]
    pub fn init_field(self, i: usize, v: Value) {
        debug_assert!(!self.obj_kind().stays_put());
        self.set_word(i, v.0);
    }

    /// Atomic view of payload word `i` (vars, atoms).
    #[inline]
    pub fn atomic_word(self, i: usize) -> &'static AtomicU64 {
        // SAFETY: word is 8-byte aligned and lives as long as the heap (NoGC).
        unsafe { &*(self.payload_ptr().add(i) as *const AtomicU64) }
    }

    #[inline]
    pub fn car(self) -> Value {
        debug_assert!(self.is_cons());
        // SAFETY: conses are [layout][car][cdr].
        unsafe { Value(*self.ptr().add(1)) }
    }

    #[inline]
    pub fn cdr(self) -> Value {
        debug_assert!(self.is_cons());
        // SAFETY: conses are [layout][car][cdr].
        unsafe { Value(*self.ptr().add(2)) }
    }

    /// Identity hash, assigned on first request and stored in the header
    /// (objects may move once the GC arrives, so addresses can't be hashes).
    pub fn identity_hash(self) -> u64 {
        if !self.is_obj() {
            return self.0;
        }
        // SAFETY: word 1 of every object is the meta word.
        let meta = unsafe { &*(self.ptr().add(1) as *const AtomicU64) };
        let h = meta.load(Ordering::Relaxed);
        if h != 0 {
            return h;
        }
        let fresh = crate::equal::mix64(NEXT_IDENTITY_HASH.fetch_add(1, Ordering::Relaxed)) | 1;
        match meta.compare_exchange(0, fresh, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => fresh,
            Err(existing) => existing,
        }
    }

    /// Human-readable type name, used in error messages.
    pub fn type_name(self) -> &'static str {
        if self.is_fixnum() {
            "Integer"
        } else if self.is_cons() {
            "List"
        } else if self.is_obj() {
            match self.obj_kind() {
                Kind::EmptyList => "List",
                Kind::Closure => "Function",
                Kind::Fiber => "Fiber",
                Kind::Instance => self.field(0).class_field(crate::clos::class::NAME).sym_name(),
                k => LAYOUTS[k as usize].name,
            }
        } else if self.is_nil() {
            "nil"
        } else if self.is_bool() {
            "Boolean"
        } else if self.is_char() {
            "Character"
        } else {
            "Immediate"
        }
    }
}
