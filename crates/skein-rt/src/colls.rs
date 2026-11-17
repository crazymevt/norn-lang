//! More collections (runtime.md §2.4): sorted maps and sets, a persistent
//! queue, and transients.
//!
//! ```text
//! Sorted:    [count][root: SNode or nil][set?]       natural (compare) order
//! SNode:     [key][val][left][right][height]          persistent AVL node
//! Queue:     [count][front: list][rear: vector]       FIFO
//! Transient: [kind][owner fiber][count][data][done]   one fiber's builder
//! ```
//!
//! The parts that compare keys (lookup, insert, remove) are in skein-vm's
//! sorted.rs, where `compare` lives; everything here works without it.

use crate::heap::Mutator;
use crate::object::{Kind, alloc, cons};
use crate::value::Value;

// ---- sorted maps and sets ------------------------------------------------------------------

pub const SORTED_COUNT: usize = 0;
pub const SORTED_ROOT: usize = 1;
pub const SORTED_SET: usize = 2;

pub const N_KEY: usize = 0;
pub const N_VAL: usize = 1;
pub const N_LEFT: usize = 2;
pub const N_RIGHT: usize = 3;
pub const N_HEIGHT: usize = 4;

pub fn sorted_new(m: &mut Mutator, count: usize, root: Value, set: bool) -> Value {
    let s = alloc(m, Kind::Sorted, 3);
    s.set_word(SORTED_COUNT, count as u64);
    s.set_field(SORTED_ROOT, root);
    s.set_word(SORTED_SET, set as u64);
    s
}

pub fn node_new(m: &mut Mutator, key: Value, val: Value, left: Value, right: Value) -> Value {
    let n = alloc(m, Kind::SNode, 5);
    n.set_field(N_KEY, key);
    n.set_field(N_VAL, val);
    n.set_field(N_LEFT, left);
    n.set_field(N_RIGHT, right);
    n.set_word(N_HEIGHT, 1 + height(left).max(height(right)));
    n
}

pub fn height(n: Value) -> u64 {
    if n.is_nil() { 0 } else { n.word(N_HEIGHT) }
}

impl Value {
    pub fn sorted_count(self) -> usize {
        self.word(SORTED_COUNT) as usize
    }
    pub fn sorted_root(self) -> Value {
        self.field(SORTED_ROOT)
    }
    pub fn sorted_is_set(self) -> bool {
        self.word(SORTED_SET) != 0
    }
}

/// Visit entries in order; stop early when `f` returns false.
pub fn sorted_each(n: Value, f: &mut dyn FnMut(Value, Value) -> bool) -> bool {
    if n.is_nil() {
        return true;
    }
    sorted_each(n.field(N_LEFT), f) && f(n.field(N_KEY), n.field(N_VAL)) && sorted_each(n.field(N_RIGHT), f)
}

/// Entries in order, as (key, value) pairs.
pub fn sorted_entries(s: Value) -> Vec<(Value, Value)> {
    let mut out = Vec::with_capacity(s.sorted_count());
    sorted_each(s.sorted_root(), &mut |k, v| {
        out.push((k, v));
        true
    });
    out
}

/// The seq items of a sorted collection: keys for a set, `[k v]` entries
/// for a map.
pub fn sorted_items(m: &mut Mutator, s: Value) -> Vec<Value> {
    let set = s.sorted_is_set();
    sorted_entries(s).into_iter().map(|(k, v)| if set { k } else { crate::vector::vec_from_slice(m, &[k, v]) }).collect()
}

// ---- the persistent queue -------------------------------------------------------------------

pub const Q_COUNT: usize = 0;
pub const Q_FRONT: usize = 1;
pub const Q_REAR: usize = 2;

pub fn queue_new(m: &mut Mutator, count: usize, front: Value, rear: Value) -> Value {
    let q = alloc(m, Kind::Queue, 3);
    q.set_word(Q_COUNT, count as u64);
    q.set_field(Q_FRONT, front);
    q.set_field(Q_REAR, rear);
    q
}

impl Value {
    pub fn queue_count(self) -> usize {
        self.word(Q_COUNT) as usize
    }
}

/// Items front to back.
pub fn queue_items(q: Value) -> Vec<Value> {
    let mut out = Vec::with_capacity(q.queue_count());
    let mut f = q.field(Q_FRONT);
    while f.is_cons() {
        out.push(f.car());
        f = f.cdr();
    }
    let r = q.field(Q_REAR);
    out.extend((0..r.vec_count()).map(|i| r.vec_nth(i)));
    out
}

/// Add to the back.
pub fn queue_conj(m: &mut Mutator, q: Value, x: Value) -> Value {
    let rear = crate::vector::vec_conj(m, q.field(Q_REAR), x);
    queue_new(m, q.queue_count() + 1, q.field(Q_FRONT), rear)
}

/// The front item, or nil.
pub fn queue_peek(q: Value) -> Value {
    let f = q.field(Q_FRONT);
    if f.is_cons() {
        f.car()
    } else {
        let r = q.field(Q_REAR);
        if r.vec_count() == 0 { Value::NIL } else { r.vec_nth(0) }
    }
}

/// Without the front item.
pub fn queue_pop(m: &mut Mutator, q: Value) -> Value {
    if q.queue_count() == 0 {
        return q;
    }
    let mut front = q.field(Q_FRONT);
    let mut rear = q.field(Q_REAR);
    if !front.is_cons() {
        // Move the rear (oldest first) into the front list.
        let items: Vec<Value> = (0..rear.vec_count()).map(|i| rear.vec_nth(i)).collect();
        front = Value::NIL;
        for &x in items.iter().rev() {
            front = cons(m, x, front);
        }
        rear = crate::runtime::rt().empty_vector;
    }
    queue_new(m, q.queue_count() - 1, front.cdr(), rear)
}

// ---- transients -----------------------------------------------------------------------------

pub const T_KIND: usize = 0; // raw: 0 vector, 1 map, 2 set
pub const T_OWNER: usize = 1;
pub const T_COUNT: usize = 2; // raw (vectors)
pub const T_DATA: usize = 3; // vector: an Array buffer; map/set: the current collection
pub const T_DONE: usize = 4; // raw: 1 after persistent!

pub const TV: u64 = 0;
pub const TM: u64 = 1;
pub const TS: u64 = 2;

pub fn transient_new(m: &mut Mutator, kind: u64, owner: Value, count: usize, data: Value) -> Value {
    let t = alloc(m, Kind::Transient, 5);
    t.set_word(T_KIND, kind);
    t.set_field(T_OWNER, owner);
    t.set_word(T_COUNT, count as u64);
    t.set_field(T_DATA, data);
    t.set_word(T_DONE, 0);
    t
}
