//! Weak references and cleanups (runtime.md §4.5).
//!
//! ```text
//! Weak: [referent (not traced)][cleanup: function or nil]
//! ```
//!
//! A `Weak` holds its referent without keeping it alive. After each
//! collection's transitive closure, `process` clears referents that died.
//!
//! **Cleanups, not finalizers.** `(add-cleanup! x f)` makes a `Weak` whose
//! cleanup `f` runs (in a fiber of its own) once `x` has been collected, in
//! the style of Java's `Cleaner` and JavaScript's `FinalizationRegistry`.
//! The dead object is never handed back, so nothing is resurrected; `f` must
//! not capture `x`, or `x` stays reachable through `f`. A `Weak` with a
//! pending cleanup is a GC root itself, so its cleanup stays alive.
//!
//! Rust can't call Skein (§2.3): due cleanups go on a ready list, and a
//! carrier spawns a fiber for each (`take_ready`).

use crate::heap::Mutator;
use crate::object::{Kind, alloc};
use crate::value::Value;
use mmtk::util::{Address, ObjectReference};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub mod layout {
    pub const REFERENT: usize = 0;
    pub const CLEANUP: usize = 1;
    pub const WORDS: usize = 2;
}

/// Every live `Weak` (a plain one is dropped once it dies or its referent does).
static REGISTRY: Mutex<Vec<Value>> = Mutex::new(Vec::new());
/// Cleanups due to run.
static READY: Mutex<Vec<Value>> = Mutex::new(Vec::new());
static ANY_READY: AtomicBool = AtomicBool::new(false);

fn is_ref(v: Value) -> bool {
    v.is_obj() || v.is_cons()
}

fn object(v: Value) -> Option<ObjectReference> {
    // SAFETY: v is a tagged pointer to an MMTk object.
    ObjectReference::from_raw_address(unsafe { Address::from_usize(v.ptr() as usize) })
}

fn reachable(v: Value) -> bool {
    object(v).is_some_and(|r| r.is_reachable())
}

/// Where the collection moved live object v to, if it did (never, until
/// objects move: M11), with v's tag.
fn moved(v: Value) -> Option<Value> {
    let to = object(v)?.get_forwarded_object()?;
    Some(Value(to.to_raw_address().as_usize() as u64 | (v.0 & crate::value::TAG_MASK)))
}

/// A weak reference to `referent`, with an optional cleanup to run once the
/// referent has been collected.
pub fn weak_new(m: &mut Mutator, referent: Value, cleanup: Value) -> Value {
    let w = alloc(m, Kind::Weak, layout::WORDS);
    w.set_field(layout::REFERENT, referent);
    w.set_field(layout::CLEANUP, cleanup);
    register(w);
    w
}

/// Track a `Weak` (made here, or loaded from an image).
pub fn register(w: Value) {
    if is_ref(w.field(layout::REFERENT)) {
        REGISTRY.lock().unwrap().push(w);
    }
}

impl Value {
    /// The referent, or nil once it has been collected.
    pub fn weak_get(self) -> Value {
        self.field(layout::REFERENT)
    }

    /// Drop a pending cleanup (it will never run). True if one was pending.
    pub fn weak_cancel(self) -> bool {
        let had = !self.field(layout::CLEANUP).is_nil();
        self.set_field(layout::CLEANUP, Value::NIL);
        had
    }
}

/// GC roots: due cleanups, and `Weak`s whose cleanup is still pending.
pub fn for_each_root(f: &mut dyn FnMut(Value)) {
    READY.lock().unwrap().iter().for_each(|&v| f(v));
    for &w in REGISTRY.lock().unwrap().iter() {
        if !w.field(layout::CLEANUP).is_nil() {
            f(w);
        }
    }
}

/// After the transitive closure (world stopped): clear dead referents and
/// queue their cleanups.
pub fn process() {
    let mut reg = REGISTRY.lock().unwrap();
    let mut ready = READY.lock().unwrap();
    reg.retain(|&w| {
        if !reachable(w) {
            return false;
        }
        let r = w.field(layout::REFERENT);
        if !is_ref(r) {
            return false;
        }
        if reachable(r) {
            // (the collector doesn't trace a referent, so it doesn't
            // rewrite it either: follow it to where it went)
            if let Some(to) = moved(r) {
                w.set_field(layout::REFERENT, to);
            }
            return true;
        }
        w.set_field(layout::REFERENT, Value::NIL);
        let c = w.field(layout::CLEANUP);
        if !c.is_nil() {
            ready.push(c);
            w.set_field(layout::CLEANUP, Value::NIL);
            ANY_READY.store(true, Ordering::Release);
        }
        false
    });
}

/// Cleanups due to run (the caller spawns a fiber for each).
pub fn take_ready() -> Vec<Value> {
    if !ANY_READY.swap(false, Ordering::AcqRel) {
        return Vec::new();
    }
    std::mem::take(&mut *READY.lock().unwrap())
}
