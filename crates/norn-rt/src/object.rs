//! Heap object model. See docs/runtime.md §2.2.
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

/// Allocate an object with `payload_words` words after the header.
#[inline]
pub fn alloc(m: &mut Mutator, kind: Kind, payload_words: usize) -> Value {
    let p = m.alloc_words(HEADER_WORDS + payload_words);
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

    #[inline]
    pub fn set_field(self, i: usize, v: Value) {
        self.set_word(i, v.0)
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
