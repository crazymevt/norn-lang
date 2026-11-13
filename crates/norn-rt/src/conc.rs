//! Heap layouts for concurrency objects (runtime.md §5). The behaviour lives in
//! norn-vm's scheduler; the runtime only defines the shapes, so the GC can scan
//! them.
//!
//! All shared mutable state lives in the heap, guarded by tiny spinlocks
//! stored in the objects themselves, so channels and fibers need no Rust-side
//! allocations, finalisers or root tables.

use crate::heap::Mutator;
use crate::object::{Kind, alloc};
use crate::value::Value;
use std::sync::atomic::{AtomicU64, Ordering};

// ---- spinlock ---------------------------------------------------------------

/// A spinlock in one heap word. Held only for a few dozen instructions, and
/// never across allocation that could block (allocation never blocks).
pub struct SpinGuard(&'static AtomicU64);

impl Drop for SpinGuard {
    fn drop(&mut self) {
        self.0.store(0, Ordering::Release);
    }
}

pub fn spin_lock(obj: Value, field: usize) -> SpinGuard {
    let w = obj.atomic_word(field);
    let mut spins = 0u32;
    while w.compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
        spins += 1;
        if spins < 64 {
            std::hint::spin_loop();
        } else {
            std::thread::yield_now();
        }
    }
    SpinGuard(w)
}

// ---- fibers -----------------------------------------------------------------

pub mod fiber {
    pub const STATE: usize = 0; //       raw atomic: see states below
    pub const SEG: usize = 1; //         current stack segment (or nil before start / after finish)
    pub const FP: usize = 2; //          top frame offset (fixnum)
    pub const MODE: usize = 3; //        raw: how to resume (see modes below)
    pub const WAKE_VALUE: usize = 4; //  value delivered on wake
    pub const WAKE_AUX: usize = 5; //    second value (alts! channel)
    pub const RESULT: usize = 6;
    pub const ERROR: usize = 7; //       error message string, or nil
    pub const TRACE: usize = 8; //       vector of backtrace strings, or nil
    pub const JOINERS: usize = 9; //     list of waiters for `await`
    pub const THUNK: usize = 10; //      function to run at start
    pub const SCOPE: usize = 11; //      owning scope, or nil
    pub const NS: usize = 12; //         raw &'static Namespace
    pub const ID: usize = 13; //         raw id
    pub const CANCEL: usize = 14; //     raw atomic: 0, or a cancel reason code
    pub const LOCK: usize = 15; //       raw spinlock for JOINERS / completion
    pub const PARK: usize = 16; //       the waiter it is parked on, or nil
    pub const WAKE_ERR: usize = 17; //   nil, reason code (fixnum), failed fiber, or string
    pub const NOTIFY: usize = 18; //     raw: root-fiber completion ticket (0 = none)
    pub const JOINING: usize = 19; //    scope it is joining, or nil
    pub const DYN: usize = 20; //        dynamic environment: map var → value (or nil)
    pub const LOC: usize = 21; //        source location being loaded ("file:line"), or nil
    pub const WORDS: usize = 22;

    /// Fields holding values (everything else is raw).
    pub const VALUE_FIELDS: &[usize] = &[
        SEG, FP, WAKE_VALUE, WAKE_AUX, RESULT, ERROR, TRACE, JOINERS, THUNK, SCOPE, PARK, WAKE_ERR, JOINING, DYN, LOC,
    ];

    // States.
    pub const QUEUED: u64 = 0;
    pub const RUNNING: u64 = 1;
    pub const PARKED: u64 = 2;
    /// Woken while still running (between registering a wait and committing
    /// the park); the owner sees this and continues instead of parking.
    pub const NOTIFIED: u64 = 3;
    pub const DONE: u64 = 4;

    // Resume modes.
    pub const START: u64 = 0;
    pub const CONTINUE: u64 = 1;
    /// Write WAKE_VALUE into the destination register of the CALL before pc.
    pub const DELIVER: u64 = 2;
    /// Return WAKE_VALUE from the current frame (the park was a tail call).
    pub const RETURN: u64 = 3;
    /// Like DELIVER/RETURN, but deliver `[WAKE_VALUE WAKE_AUX]` (alts!).
    pub const DELIVER_PAIR: u64 = 4;
    pub const RETURN_PAIR: u64 = 5;

    // Cancel / wake-error reason codes.
    pub const CANCELLED: i64 = 1;
    pub const INTERRUPTED: i64 = 2;
}

pub fn fiber_new(m: &mut Mutator, id: u64, thunk: Value, scope: Value, ns: usize) -> Value {
    use fiber::*;
    let f = alloc(m, Kind::Fiber, WORDS);
    for &i in VALUE_FIELDS {
        f.set_field(i, Value::NIL);
    }
    f.set_field(FP, Value::fix(0));
    f.set_word(STATE, QUEUED);
    f.set_word(MODE, START);
    f.set_field(THUNK, thunk);
    f.set_field(SCOPE, scope);
    f.set_word(NS, ns as u64);
    f.set_word(ID, id);
    f.set_word(CANCEL, 0);
    f.set_word(LOCK, 0);
    f.set_word(NOTIFY, 0);
    f
}

impl Value {
    pub fn fiber_id(self) -> u64 {
        self.word(fiber::ID)
    }
    #[inline]
    pub fn fiber_state(self) -> &'static AtomicU64 {
        self.atomic_word(fiber::STATE)
    }
    #[inline]
    pub fn fiber_cancel(self) -> u64 {
        self.atomic_word(fiber::CANCEL).load(Ordering::SeqCst)
    }
}

// ---- claims and waiters -------------------------------------------------------

/// `Claim: [flag]` — whoever CASes 0→1 owns the right to wake the waiter(s).
/// An `alts!` shares one claim between its waiters on several channels.
pub fn claim_new(m: &mut Mutator) -> Value {
    let c = alloc(m, Kind::Claim, 1);
    c.set_word(0, 0);
    c
}

pub fn try_claim(claim: Value) -> bool {
    claim.atomic_word(0).compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed).is_ok()
}

pub fn is_claimed(claim: Value) -> bool {
    claim.atomic_word(0).load(Ordering::Acquire) != 0
}

pub mod waiter {
    pub const FIBER: usize = 0;
    pub const VALUE: usize = 1; // value offered by a parked putter
    pub const CLAIM: usize = 2;
    pub const CHAN: usize = 3; // channel (reported by alts!)
    pub const FLAGS: usize = 4; // raw
    pub const WORDS: usize = 5;
    /// Cancellation does not wake this waiter (scope joins: the owner waits
    /// for its cancelled children instead).
    pub const NO_CANCEL_WAKE: u64 = 1;
    /// `await` without re-raising the awaited fiber's error (`%join`).
    pub const NO_RAISE: u64 = 2;
}

pub fn waiter_new(m: &mut Mutator, fiber: Value, value: Value, claim: Value, chan: Value, flags: u64) -> Value {
    let w = alloc(m, Kind::Waiter, waiter::WORDS);
    w.set_field(waiter::FIBER, fiber);
    w.set_field(waiter::VALUE, value);
    w.set_field(waiter::CLAIM, claim);
    w.set_field(waiter::CHAN, chan);
    w.set_word(waiter::FLAGS, flags);
    w
}

// ---- channels ------------------------------------------------------------------

pub mod chan {
    pub const LOCK: usize = 0; // raw spinlock
    pub const CAP: usize = 1; // raw buffer capacity
    pub const CLOSED: usize = 2; // raw 0/1
    pub const BUF: usize = 3; // Array ring buffer, or nil when CAP == 0
    pub const HEAD: usize = 4; // raw
    pub const LEN: usize = 5; // raw
    pub const TAKERS_FRONT: usize = 6; // FIFO of waiters as two lists
    pub const TAKERS_BACK: usize = 7;
    pub const PUTTERS_FRONT: usize = 8;
    pub const PUTTERS_BACK: usize = 9;
    pub const WORDS: usize = 10;
    pub const VALUE_FIELDS: &[usize] = &[BUF, TAKERS_FRONT, TAKERS_BACK, PUTTERS_FRONT, PUTTERS_BACK];
}

pub fn chan_new(m: &mut Mutator, cap: usize) -> Value {
    use chan::*;
    let buf = if cap > 0 { crate::array::array_new(m, cap, Value::NIL) } else { Value::NIL };
    let c = alloc(m, Kind::Chan, WORDS);
    c.set_word(LOCK, 0);
    c.set_word(CAP, cap as u64);
    c.set_word(CLOSED, 0);
    c.set_field(BUF, buf);
    c.set_word(HEAD, 0);
    c.set_word(LEN, 0);
    for f in [TAKERS_FRONT, TAKERS_BACK, PUTTERS_FRONT, PUTTERS_BACK] {
        c.set_field(f, Value::NIL);
    }
    c
}

// ---- scopes ----------------------------------------------------------------------

pub mod scope {
    pub const LOCK: usize = 0; // raw spinlock
    pub const CHILDREN: usize = 1; // list of fibers
    pub const PENDING: usize = 2; // raw count of unfinished children
    pub const FAILED: usize = 3; // first child that failed, or nil
    pub const OWNER: usize = 4; // waiter of the joining owner, or nil
    pub const WORDS: usize = 5;
    pub const VALUE_FIELDS: &[usize] = &[CHILDREN, FAILED, OWNER];
}

pub fn scope_new(m: &mut Mutator) -> Value {
    use scope::*;
    let s = alloc(m, Kind::Scope, WORDS);
    s.set_word(LOCK, 0);
    s.set_field(CHILDREN, Value::NIL);
    s.set_word(PENDING, 0);
    s.set_field(FAILED, Value::NIL);
    s.set_field(OWNER, Value::NIL);
    s
}

// ---- atoms -------------------------------------------------------------------------

pub fn atom_new(m: &mut Mutator, v: Value) -> Value {
    let a = alloc(m, Kind::Atom, 1);
    a.set_field(0, v);
    a
}

impl Value {
    #[inline]
    pub fn atom_get(self) -> Value {
        Value(self.atomic_word(0).load(Ordering::Acquire))
    }
    #[inline]
    pub fn atom_set(self, v: Value) {
        self.atomic_word(0).store(v.0, Ordering::Release)
    }
    /// Identity compare-and-set.
    #[inline]
    pub fn atom_cas(self, old: Value, new: Value) -> bool {
        self.atomic_word(0).compare_exchange(old.0, new.0, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }
}

// ---- reader state --------------------------------------------------------------------

/// `Reader: [source string][byte position][source name][spare]` — lets Norn
/// code read forms one at a time from a string (`load-string`, the REPL).
pub fn reader_new(m: &mut Mutator, src: Value, name: Value) -> Value {
    let r = alloc(m, Kind::Reader, 4);
    r.set_field(0, src);
    r.set_word(1, 0);
    r.set_field(2, name);
    r.set_word(3, 0);
    r
}
