//! Lists and lightweight sequences.
//!
//! - Cons chains end in `nil`; the empty list `()` is a distinct singleton
//!   (`()` is not `nil`, design.md §2).
//! - Range:  `[start][end][step]` — a reducible, indexed sequence of fixnums.
//! - VecSeq: `[vector][index]` — a seq view over a vector, no copying.

use crate::heap::Mutator;
use crate::object::{Kind, alloc, cons};
use crate::runtime::rt;
use crate::value::Value;

/// Build a list from a slice. Empty slice → `()`.
pub fn list_from(m: &mut Mutator, xs: &[Value]) -> Value {
    if xs.is_empty() {
        return rt().empty_list;
    }
    list_from_with_tail(m, xs, Value::NIL)
}

/// Build `(x0 x1 … . tail)`. `tail` must be nil or a seq.
pub fn list_from_with_tail(m: &mut Mutator, xs: &[Value], tail: Value) -> Value {
    let mut acc = tail;
    for &x in xs.iter().rev() {
        acc = cons(m, x, acc);
    }
    acc
}

pub fn range_new(m: &mut Mutator, start: i64, end: i64, step: i64) -> Value {
    let r = alloc(m, Kind::Range, 3);
    r.set_word(0, start as u64);
    r.set_word(1, end as u64);
    r.set_word(2, step as u64);
    r
}

/// Number of elements in a range with the given bounds.
pub fn range_len(start: i64, end: i64, step: i64) -> usize {
    if step > 0 && start < end {
        ((end - start) as u128).div_ceil(step as u128) as usize
    } else if step < 0 && start > end {
        ((start - end) as u128).div_ceil((-step) as u128) as usize
    } else {
        0
    }
}

pub fn vecseq_new(m: &mut Mutator, vec: Value, index: usize) -> Value {
    let s = alloc(m, Kind::VecSeq, 2);
    s.set_field(0, vec);
    s.set_word(1, index as u64);
    s
}

impl Value {
    pub fn range_parts(self) -> (i64, i64, i64) {
        (self.word(0) as i64, self.word(1) as i64, self.word(2) as i64)
    }

    pub fn range_count(self) -> usize {
        let (s, e, st) = self.range_parts();
        range_len(s, e, st)
    }

    pub fn vecseq_parts(self) -> (Value, usize) {
        (self.field(0), self.word(1) as usize)
    }
}
