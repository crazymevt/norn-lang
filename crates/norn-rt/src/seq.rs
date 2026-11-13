//! The sequence abstraction: `seq`, `first`, `rest`, `next`, `count`, `nth`,
//! plus an allocation-free Rust iterator over sequential collections.
//!
//! **Streams** (design §3.3) are lazy seqs: `[state][value]`, unrealized
//! (value = a thunk) or realized (value = the seq it produced, or nil).
//! Rust never runs the thunk. An operation that reaches an unrealized
//! stream fails, and the interpreter forces the stream (calling Norn's
//! `%force-stream`) and runs the operation again (runtime.md §5.4).

use crate::heap::Mutator;
use crate::list::{list_from, range_new, vecseq_new};
use crate::object::{Kind, cons};
use crate::runtime::rt;
use crate::value::Value;
use crate::vector::vec_from_slice;

pub const STREAM_STATE: usize = 0;
pub const STREAM_VALUE: usize = 1;
pub const UNREALIZED: u64 = 0;
pub const REALIZED: u64 = 1;

/// A stream that will produce its seq by calling `thunk`.
pub fn stream_new(m: &mut Mutator, thunk: Value) -> Value {
    let s = crate::object::alloc(m, Kind::Stream, 2);
    s.set_word(STREAM_STATE, UNREALIZED);
    s.set_field(STREAM_VALUE, thunk);
    s
}

impl Value {
    pub fn stream_realized(self) -> bool {
        self.atomic_word(STREAM_STATE).load(std::sync::atomic::Ordering::Acquire) == REALIZED
    }
    /// The seq a realized stream holds.
    pub fn stream_value(self) -> Value {
        self.field(STREAM_VALUE)
    }
}

/// Realize `s` with `v` (unless another fiber got there first). Returns the
/// realized value.
pub fn stream_set(s: Value, v: Value) -> Value {
    use std::sync::atomic::Ordering;
    let state = s.atomic_word(STREAM_STATE);
    // Mark it as being set (2) so only one writer stores the value.
    if state.compare_exchange(UNREALIZED, 2, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        s.set_field(STREAM_VALUE, v);
        state.store(REALIZED, Ordering::Release);
        return v;
    }
    while state.load(Ordering::Acquire) != REALIZED {
        std::hint::spin_loop();
    }
    s.stream_value()
}

/// The first unrealized stream reached by following `v` as a seq (through
/// realized streams and cons tails), if any.
pub fn first_unrealized(mut v: Value) -> Option<Value> {
    loop {
        if v.is_cons() {
            v = v.cdr();
        } else if v.is(Kind::Stream) {
            if !v.stream_realized() {
                return Some(v);
            }
            v = v.stream_value();
        } else {
            return None;
        }
    }
}

/// A realized stream's items as a list (for printing).
pub fn stream_items(v: Value) -> Value {
    let mut cur = v;
    while cur.is(Kind::Stream) {
        cur = cur.stream_value();
    }
    cur
}

/// Iterates lists, vectors, vector seqs and ranges without allocating.
pub enum SeqIter {
    List(Value),
    Vec { v: Value, i: usize, n: usize },
    Range { cur: i64, left: usize, step: i64 },
    Done,
}

impl SeqIter {
    /// `None` if `v` isn't sequential (maps, sets and strings need `seq`).
    pub fn new(v: Value) -> Option<SeqIter> {
        if v.is_nil() {
            return Some(SeqIter::Done);
        }
        if v.is_cons() || v.is(Kind::Stream) {
            // A chain with an unrealized stream in it can't be walked here.
            if first_unrealized(v).is_some() {
                return None;
            }
            return Some(if v.is_cons() { SeqIter::List(v) } else { return SeqIter::new(stream_items(v)) });
        }
        if !v.is_obj() {
            return None;
        }
        Some(match v.obj_kind() {
            Kind::EmptyList => SeqIter::Done,
            Kind::Vector => SeqIter::Vec { v, i: 0, n: v.vec_count() },
            Kind::VecSeq => {
                let (vec, i) = v.vecseq_parts();
                SeqIter::Vec { v: vec, i, n: vec.vec_count() }
            }
            Kind::Range => {
                let (s, _, step) = v.range_parts();
                SeqIter::Range { cur: s, left: v.range_count(), step }
            }
            _ => return None,
        })
    }
}

impl Iterator for SeqIter {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        loop {
            match self {
                SeqIter::Done => return None,
                SeqIter::List(v) => {
                    let cur = *v;
                    if cur.is_cons() {
                        *self = SeqIter::List(cur.cdr());
                        return Some(cur.car());
                    }
                    // A cons whose tail is some other kind of seq (a realized
                    // stream's items included).
                    *self = SeqIter::new(stream_items(cur)).unwrap_or(SeqIter::Done);
                }
                SeqIter::Vec { v, i, n } => {
                    if *i >= *n {
                        return None;
                    }
                    let x = v.vec_nth(*i);
                    *i += 1;
                    return Some(x);
                }
                SeqIter::Range { cur, left, step } => {
                    if *left == 0 {
                        return None;
                    }
                    let x = Value::fix(*cur);
                    *cur += *step;
                    *left -= 1;
                    return Some(x);
                }
            }
        }
    }
}

/// Collect any seqable into a Rust Vec (maps yield `[k v]` entry vectors).
pub fn to_vec(m: &mut Mutator, v: Value) -> Result<Vec<Value>, String> {
    if let Some(it) = SeqIter::new(v) {
        return Ok(it.collect());
    }
    let mut out = Vec::new();
    match v.obj_kind() {
        Kind::Map => {
            let mut pairs = Vec::with_capacity(v.map_count());
            v.map_for_each(|k, val| {
                pairs.push((k, val));
                true
            });
            for (k, val) in pairs {
                out.push(vec_from_slice(m, &[k, val]));
            }
        }
        Kind::Set => {
            v.set_for_each(|k| {
                out.push(k);
                true
            });
        }
        Kind::String => out.extend(v.as_str().chars().map(Value::char)),
        Kind::Bytes => out.extend(v.as_bytes().iter().map(|&b| Value::fix(b as i64))),
        _ => return Err(format!("Don't know how to create a seq from {}", v.type_name())),
    }
    Ok(out)
}

pub fn is_seqable(v: Value) -> bool {
    v.is_nil()
        || v.is_cons()
        || (v.is_obj()
            && matches!(
                v.obj_kind(),
                Kind::EmptyList
                    | Kind::Vector
                    | Kind::VecSeq
                    | Kind::Range
                    | Kind::Map
                    | Kind::Set
                    | Kind::String
                    | Kind::Bytes
                    | Kind::Stream
            ))
}

/// `(seq x)`: nil for empty collections, otherwise a seq.
pub fn seq(m: &mut Mutator, v: Value) -> Result<Value, String> {
    if v.is_nil() || v.is_cons() {
        return Ok(v);
    }
    if !v.is_obj() {
        return Err(format!("Don't know how to create a seq from {}", v.type_name()));
    }
    Ok(match v.obj_kind() {
        Kind::EmptyList => Value::NIL,
        Kind::Vector => {
            if v.vec_count() == 0 {
                Value::NIL
            } else {
                vecseq_new(m, v, 0)
            }
        }
        Kind::VecSeq => v,
        Kind::Stream => {
            if !v.stream_realized() {
                return Err(NEEDS_FORCE.into());
            }
            seq(m, v.stream_value())?
        }
        Kind::Range => {
            if v.range_count() == 0 {
                Value::NIL
            } else {
                v
            }
        }
        _ => {
            let xs = to_vec(m, v)?;
            if xs.is_empty() { Value::NIL } else { list_from(m, &xs) }
        }
    })
}

/// The error message of an operation that met an unrealized stream (the
/// interpreter forces it and tries again).
pub const NEEDS_FORCE: &str = "A stream needs to be realized";

pub fn first(m: &mut Mutator, v: Value) -> Result<Value, String> {
    if v.is_nil() {
        return Ok(Value::NIL);
    }
    if v.is_cons() {
        return Ok(v.car());
    }
    if v.is_obj() {
        match v.obj_kind() {
            Kind::EmptyList => return Ok(Value::NIL),
            Kind::Vector => return Ok(if v.vec_count() == 0 { Value::NIL } else { v.vec_nth(0) }),
            Kind::VecSeq => {
                let (vec, i) = v.vecseq_parts();
                return Ok(vec.vec_nth(i));
            }
            Kind::Range => return Ok(if v.range_count() == 0 { Value::NIL } else { Value::fix(v.range_parts().0) }),
            _ => {}
        }
    }
    let s = seq(m, v)?;
    first(m, s)
}

pub fn rest(m: &mut Mutator, v: Value) -> Result<Value, String> {
    let empty = rt().empty_list;
    if v.is_nil() {
        return Ok(empty);
    }
    if v.is_cons() {
        let d = v.cdr();
        return Ok(if d.is_nil() { empty } else { d });
    }
    if v.is_obj() {
        match v.obj_kind() {
            Kind::EmptyList => return Ok(empty),
            Kind::Vector => return Ok(if v.vec_count() <= 1 { empty } else { vecseq_new(m, v, 1) }),
            Kind::VecSeq => {
                let (vec, i) = v.vecseq_parts();
                return Ok(if i + 1 >= vec.vec_count() { empty } else { vecseq_new(m, vec, i + 1) });
            }
            Kind::Range => {
                let (s, e, st) = v.range_parts();
                return Ok(if v.range_count() <= 1 { empty } else { range_new(m, s + st, e, st) });
            }
            _ => {}
        }
    }
    let s = seq(m, v)?;
    if s.is_nil() { Ok(empty) } else { rest(m, s) }
}

pub fn next(m: &mut Mutator, v: Value) -> Result<Value, String> {
    let r = rest(m, v)?;
    seq(m, r)
}

/// Prepend `x` to any seqable. A stream tail stays as it is, unrealized
/// (forcing it here would make every lazy cons chain eager).
pub fn cons_onto(m: &mut Mutator, x: Value, coll: Value) -> Result<Value, String> {
    if coll.is(Kind::Stream) {
        return Ok(cons(m, x, coll));
    }
    let s = seq(m, coll)?;
    Ok(cons(m, x, s))
}

pub fn count(v: Value) -> Result<usize, String> {
    if v.is_nil() {
        return Ok(0);
    }
    if v.is_cons() {
        let mut n = 0;
        let mut cur = v;
        while cur.is_cons() {
            n += 1;
            cur = cur.cdr();
        }
        return if cur.is_nil() { Ok(n) } else { Ok(n + count(cur)?) };
    }
    if v.is_obj() {
        return Ok(match v.obj_kind() {
            Kind::EmptyList => 0,
            Kind::Vector => v.vec_count(),
            Kind::VecSeq => {
                let (vec, i) = v.vecseq_parts();
                vec.vec_count() - i
            }
            Kind::Range => v.range_count(),
            Kind::Map => v.map_count(),
            Kind::Set => v.set_count(),
            Kind::String => v.as_str().chars().count(),
            Kind::Bytes => v.as_bytes().len(),
            Kind::Stream => match SeqIter::new(v) {
                Some(it) => it.count(),
                None => return Err(NEEDS_FORCE.into()),
            },
            _ => return Err(format!("count not supported on {}", v.type_name())),
        });
    }
    Err(format!("count not supported on {}", v.type_name()))
}

/// Element `i`, `Ok(None)` when out of bounds.
pub fn nth(v: Value, i: usize) -> Result<Option<Value>, String> {
    if v.is_nil() {
        return Ok(None);
    }
    if v.is_obj() {
        match v.obj_kind() {
            Kind::Vector => return Ok((i < v.vec_count()).then(|| v.vec_nth(i))),
            Kind::VecSeq => {
                let (vec, j) = v.vecseq_parts();
                return Ok((i + j < vec.vec_count()).then(|| vec.vec_nth(i + j)));
            }
            Kind::Range => {
                let (s, _, st) = v.range_parts();
                return Ok((i < v.range_count()).then(|| Value::fix(s + st * i as i64)));
            }
            Kind::String => return Ok(v.as_str().chars().nth(i).map(Value::char)),
            Kind::Bytes => return Ok(v.as_bytes().get(i).map(|&b| Value::fix(b as i64))),
            Kind::EmptyList => return Ok(None),
            _ => {}
        }
    }
    if v.is_cons() || v.is(Kind::Stream) {
        // Step along the chain: a stream only has to be realized as far as
        // index i (it may be infinite).
        let mut cur = v;
        let mut k = i;
        loop {
            if cur.is(Kind::Stream) {
                if !cur.stream_realized() {
                    return Err(NEEDS_FORCE.into());
                }
                cur = cur.stream_value();
            } else if cur.is_cons() {
                if k == 0 {
                    return Ok(Some(cur.car()));
                }
                k -= 1;
                cur = cur.cdr();
            } else {
                return match SeqIter::new(cur) {
                    Some(mut it) => Ok(it.nth(k)),
                    None => Err(format!("nth not supported on {}", cur.type_name())),
                };
            }
        }
    }
    match SeqIter::new(v) {
        Some(mut it) => Ok(it.nth(i)),
        None => Err(format!("nth not supported on {}", v.type_name())),
    }
}

/// Supports O(1)/O(log n) `count` and `nth` (used by `reduce`'s fast path).
pub fn is_indexed(v: Value) -> bool {
    v.is_obj() && matches!(v.obj_kind(), Kind::Vector | Kind::VecSeq | Kind::Range)
}
