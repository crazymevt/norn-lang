//! Sorted maps and sets: the persistent AVL tree's operations that compare
//! keys (the structure is in norn-rt colls.rs). Keys are ordered by
//! `compare`, so every key in one collection must be comparable with the
//! others (numbers with numbers, strings with strings…).

use crate::prims::compare;
use crate::vm::{Error, Result};
use norn_rt::colls::{N_KEY, N_LEFT, N_RIGHT, N_VAL, height, node_new, sorted_new};
use norn_rt::{Kind, Mutator, Value};
use std::cmp::Ordering;

fn balance(n: Value) -> i64 {
    height(n.field(N_LEFT)) as i64 - height(n.field(N_RIGHT)) as i64
}

fn with_children(m: &mut Mutator, n: Value, left: Value, right: Value) -> Value {
    node_new(m, n.field(N_KEY), n.field(N_VAL), left, right)
}

fn rotate_right(m: &mut Mutator, n: Value) -> Value {
    let l = n.field(N_LEFT);
    let n2 = with_children(m, n, l.field(N_RIGHT), n.field(N_RIGHT));
    with_children(m, l, l.field(N_LEFT), n2)
}

fn rotate_left(m: &mut Mutator, n: Value) -> Value {
    let r = n.field(N_RIGHT);
    let n2 = with_children(m, n, n.field(N_LEFT), r.field(N_LEFT));
    with_children(m, r, n2, r.field(N_RIGHT))
}

/// Restore the AVL invariant at `n` (its children are balanced).
fn rebalance(m: &mut Mutator, n: Value) -> Value {
    let b = balance(n);
    if b > 1 {
        let l = n.field(N_LEFT);
        let n = if balance(l) < 0 {
            let l2 = rotate_left(m, l);
            with_children(m, n, l2, n.field(N_RIGHT))
        } else {
            n
        };
        rotate_right(m, n)
    } else if b < -1 {
        let r = n.field(N_RIGHT);
        let n = if balance(r) > 0 {
            let r2 = rotate_right(m, r);
            with_children(m, n, n.field(N_LEFT), r2)
        } else {
            n
        };
        rotate_left(m, n)
    } else {
        n
    }
}

/// The node holding `key`, if any.
pub(crate) fn find(mut n: Value, key: Value) -> Result<Option<Value>> {
    while !n.is_nil() {
        n = match compare(key, n.field(N_KEY))? {
            Ordering::Less => n.field(N_LEFT),
            Ordering::Greater => n.field(N_RIGHT),
            Ordering::Equal => return Ok(Some(n)),
        };
    }
    Ok(None)
}

/// `n` with `key` → `val` (replacing an existing value). The flag says
/// whether the key is new.
fn insert(m: &mut Mutator, n: Value, key: Value, val: Value) -> Result<(Value, bool)> {
    if n.is_nil() {
        return Ok((node_new(m, key, val, Value::NIL, Value::NIL), true));
    }
    Ok(match compare(key, n.field(N_KEY))? {
        Ordering::Equal => (node_new(m, key, val, n.field(N_LEFT), n.field(N_RIGHT)), false),
        Ordering::Less => {
            let (l, added) = insert(m, n.field(N_LEFT), key, val)?;
            let n2 = with_children(m, n, l, n.field(N_RIGHT));
            (rebalance(m, n2), added)
        }
        Ordering::Greater => {
            let (r, added) = insert(m, n.field(N_RIGHT), key, val)?;
            let n2 = with_children(m, n, n.field(N_LEFT), r);
            (rebalance(m, n2), added)
        }
    })
}

/// The leftmost node and `n` without it.
fn take_min(m: &mut Mutator, n: Value) -> (Value, Value) {
    let l = n.field(N_LEFT);
    if l.is_nil() {
        return (n, n.field(N_RIGHT));
    }
    let (min, l2) = take_min(m, l);
    let n2 = with_children(m, n, l2, n.field(N_RIGHT));
    (min, rebalance(m, n2))
}

/// `n` without `key`. The flag says whether it was there.
fn remove(m: &mut Mutator, n: Value, key: Value) -> Result<(Value, bool)> {
    if n.is_nil() {
        return Ok((n, false));
    }
    Ok(match compare(key, n.field(N_KEY))? {
        Ordering::Less => {
            let (l, gone) = remove(m, n.field(N_LEFT), key)?;
            if !gone {
                return Ok((n, false));
            }
            let n2 = with_children(m, n, l, n.field(N_RIGHT));
            (rebalance(m, n2), true)
        }
        Ordering::Greater => {
            let (r, gone) = remove(m, n.field(N_RIGHT), key)?;
            if !gone {
                return Ok((n, false));
            }
            let n2 = with_children(m, n, n.field(N_LEFT), r);
            (rebalance(m, n2), true)
        }
        Ordering::Equal => {
            let (l, r) = (n.field(N_LEFT), n.field(N_RIGHT));
            if l.is_nil() {
                (r, true)
            } else if r.is_nil() {
                (l, true)
            } else {
                let (min, r2) = take_min(m, r);
                let n2 = node_new(m, min.field(N_KEY), min.field(N_VAL), l, r2);
                (rebalance(m, n2), true)
            }
        }
    })
}

pub(crate) fn get(s: Value, key: Value) -> Result<Option<Value>> {
    Ok(find(s.sorted_root(), key)?.map(|n| n.field(N_VAL)))
}

pub(crate) fn assoc(m: &mut Mutator, s: Value, key: Value, val: Value) -> Result<Value> {
    let (root, added) = insert(m, s.sorted_root(), key, val)?;
    Ok(sorted_new(m, s.sorted_count() + usize::from(added), root, s.sorted_is_set()))
}

pub(crate) fn dissoc(m: &mut Mutator, s: Value, key: Value) -> Result<Value> {
    let (root, gone) = remove(m, s.sorted_root(), key)?;
    Ok(if gone { sorted_new(m, s.sorted_count() - 1, root, s.sorted_is_set()) } else { s })
}

/// `(subseq coll test key)` and with a second bound: the items within the
/// bounds, in order (or reverse order), as a vector. Tests are `<`, `<=`,
/// `>`, `>=` given as keywords.
pub(crate) fn subseq(m: &mut Mutator, s: Value, bounds: &[(Ordering, bool, Value)], reverse: bool) -> Result<Value> {
    let set = s.sorted_is_set();
    let mut out = Vec::new();
    let mut err = None;
    norn_rt::colls::sorted_each(s.sorted_root(), &mut |k, v| {
        for &(side, inclusive, b) in bounds {
            match compare(k, b) {
                Ok(o) => {
                    if !(o == side || (inclusive && o == Ordering::Equal)) {
                        return true;
                    }
                }
                Err(e) => {
                    err = Some(e);
                    return false;
                }
            }
        }
        out.push(if set { k } else { norn_rt::vector::vec_from_slice(m, &[k, v]) });
        true
    });
    if let Some(e) = err {
        return Err(e);
    }
    if reverse {
        out.reverse();
    }
    Ok(norn_rt::vector::vec_from_slice(m, &out))
}

pub(crate) fn check(s: Value, op: &str) -> Result<()> {
    if s.is(Kind::Sorted) { Ok(()) } else { Err(Error::new(format!("{op} expects a sorted map or set, got {}", s.type_name()))) }
}
