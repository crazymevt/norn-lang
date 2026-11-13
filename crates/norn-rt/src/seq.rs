//! The sequence abstraction: `seq`, `first`, `rest`, `next`, `count`, `nth`,
//! plus an allocation-free Rust iterator over sequential collections.

use crate::heap::Mutator;
use crate::list::{list_from, range_new, vecseq_new};
use crate::object::{Kind, cons};
use crate::runtime::rt;
use crate::value::Value;
use crate::vector::vec_from_slice;

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
        if v.is_cons() {
            return Some(SeqIter::List(v));
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
                    // A cons whose tail is some other kind of seq.
                    *self = SeqIter::new(cur).unwrap_or(SeqIter::Done);
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

/// Prepend `x` to any seqable.
pub fn cons_onto(m: &mut Mutator, x: Value, coll: Value) -> Result<Value, String> {
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
    match SeqIter::new(v) {
        Some(mut it) => Ok(it.nth(i)),
        None => Err(format!("nth not supported on {}", v.type_name())),
    }
}

/// Supports O(1)/O(log n) `count` and `nth` (used by `reduce`'s fast path).
pub fn is_indexed(v: Value) -> bool {
    v.is_obj() && matches!(v.obj_kind(), Kind::Vector | Kind::VecSeq | Kind::Range)
}
