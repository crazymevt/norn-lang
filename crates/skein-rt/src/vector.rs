//! Persistent vector: a 32-way trie with a tail (Clojure's PersistentVector).
//! RRB concatenation comes later; the layout is compatible with adding it.
//!
//! `Vector: [count][shift][root: Array][tail: Array]`
//!
//! Interior nodes and leaves are `Array`s holding only their live children,
//! so the tree is always left-packed and leaves are always full.

use crate::array::{array_copy, array_from, array_resized};
use crate::heap::Mutator;
use crate::object::{Kind, alloc};
use crate::runtime::rt;
use crate::value::Value;

const BITS: usize = 5;
const WIDTH: usize = 1 << BITS;
const MASK: usize = WIDTH - 1;

const CNT: usize = 0;
const SHIFT: usize = 1;
const ROOT: usize = 2;
const TAIL: usize = 3;

fn mk(m: &mut Mutator, cnt: usize, shift: usize, root: Value, tail: Value) -> Value {
    let v = alloc(m, Kind::Vector, 4);
    v.set_word(CNT, cnt as u64);
    v.set_word(SHIFT, shift as u64);
    v.set_field(ROOT, root);
    v.set_field(TAIL, tail);
    v
}

/// Used once by the runtime to create the shared empty vector.
pub(crate) fn vec_make_empty(m: &mut Mutator, empty_array: Value) -> Value {
    mk(m, 0, BITS, empty_array, empty_array)
}

#[inline]
fn tailoff(cnt: usize) -> usize {
    if cnt < WIDTH { 0 } else { ((cnt - 1) >> BITS) << BITS }
}

impl Value {
    #[inline]
    pub fn vec_count(self) -> usize {
        self.word(CNT) as usize
    }

    fn vec_shift(self) -> usize {
        self.word(SHIFT) as usize
    }

    /// The leaf array that holds index `i`.
    fn vec_leaf_for(self, i: usize) -> Value {
        if i >= tailoff(self.vec_count()) {
            return self.field(TAIL);
        }
        let mut node = self.field(ROOT);
        let mut level = self.vec_shift();
        while level > 0 {
            node = node.arr_get((i >> level) & MASK);
            level -= BITS;
        }
        node
    }

    /// Element `i`. Caller checks bounds.
    #[inline]
    pub fn vec_nth(self, i: usize) -> Value {
        debug_assert!(i < self.vec_count());
        self.vec_leaf_for(i).arr_get(i & MASK)
    }

    /// Visit elements in order; stop early when `f` returns false.
    pub fn vec_for_each(self, mut f: impl FnMut(Value) -> bool) -> bool {
        let cnt = self.vec_count();
        let mut i = 0;
        while i < cnt {
            let leaf = self.vec_leaf_for(i);
            for &x in leaf.arr_slice() {
                if !f(x) {
                    return false;
                }
            }
            i += leaf.arr_len();
        }
        true
    }
}

pub fn vec_empty() -> Value {
    rt().empty_vector
}

pub fn vec_conj(m: &mut Mutator, v: Value, x: Value) -> Value {
    let cnt = v.vec_count();
    let shift = v.vec_shift();
    let tail = v.field(TAIL);
    if cnt - tailoff(cnt) < WIDTH {
        let new_tail = array_resized(m, tail, tail.arr_len() + 1);
        new_tail.arr_set(tail.arr_len(), x);
        return mk(m, cnt + 1, shift, v.field(ROOT), new_tail);
    }
    // Tail is full: push it into the tree.
    let root = v.field(ROOT);
    let (new_root, new_shift) = if (cnt >> BITS) > (1 << shift) {
        let path = new_path(m, shift, tail);
        (array_from(m, &[root, path]), shift + BITS)
    } else {
        (push_tail(m, cnt, shift, root, tail), shift)
    };
    let new_tail = array_from(m, &[x]);
    mk(m, cnt + 1, new_shift, new_root, new_tail)
}

fn new_path(m: &mut Mutator, level: usize, node: Value) -> Value {
    if level == 0 {
        node
    } else {
        let child = new_path(m, level - BITS, node);
        array_from(m, &[child])
    }
}

fn push_tail(m: &mut Mutator, cnt: usize, level: usize, parent: Value, tail: Value) -> Value {
    let subidx = ((cnt - 1) >> level) & MASK;
    let len = parent.arr_len().max(subidx + 1);
    let ret = array_resized(m, parent, len);
    let insert = if level == BITS {
        tail
    } else if subidx < parent.arr_len() {
        push_tail(m, cnt, level - BITS, parent.arr_get(subidx), tail)
    } else {
        new_path(m, level - BITS, tail)
    };
    ret.arr_set(subidx, insert);
    ret
}

/// Replace element `i` (`i == count` appends). Caller checks bounds.
pub fn vec_assoc(m: &mut Mutator, v: Value, i: usize, x: Value) -> Value {
    let cnt = v.vec_count();
    if i == cnt {
        return vec_conj(m, v, x);
    }
    if i >= tailoff(cnt) {
        let t = array_copy(m, v.field(TAIL));
        t.arr_set(i & MASK, x);
        return mk(m, cnt, v.vec_shift(), v.field(ROOT), t);
    }
    let root = do_assoc(m, v.vec_shift(), v.field(ROOT), i, x);
    mk(m, cnt, v.vec_shift(), root, v.field(TAIL))
}

fn do_assoc(m: &mut Mutator, level: usize, node: Value, i: usize, x: Value) -> Value {
    let ret = array_copy(m, node);
    if level == 0 {
        ret.arr_set(i & MASK, x);
    } else {
        let s = (i >> level) & MASK;
        let child = do_assoc(m, level - BITS, node.arr_get(s), i, x);
        ret.arr_set(s, child);
    }
    ret
}

/// Remove the last element. Caller checks the vector is non-empty.
pub fn vec_pop(m: &mut Mutator, v: Value) -> Value {
    let cnt = v.vec_count();
    debug_assert!(cnt > 0);
    if cnt == 1 {
        return vec_empty();
    }
    let tail = v.field(TAIL);
    if cnt - tailoff(cnt) > 1 {
        let t = array_resized(m, tail, tail.arr_len() - 1);
        return mk(m, cnt - 1, v.vec_shift(), v.field(ROOT), t);
    }
    let new_tail = v.vec_leaf_for(cnt - 2);
    let mut shift = v.vec_shift();
    let mut root = match pop_tail(m, cnt, shift, v.field(ROOT)) {
        Some(r) => r,
        None => rt().empty_array,
    };
    if shift > BITS && root.arr_len() == 1 {
        root = root.arr_get(0);
        shift -= BITS;
    }
    mk(m, cnt - 1, shift, root, new_tail)
}

fn pop_tail(m: &mut Mutator, cnt: usize, level: usize, node: Value) -> Option<Value> {
    let subidx = ((cnt - 2) >> level) & MASK;
    if level > BITS {
        let child = pop_tail(m, cnt, level - BITS, node.arr_get(subidx));
        match child {
            None if subidx == 0 => None,
            None => Some(array_resized(m, node, subidx)),
            Some(c) => {
                let ret = array_copy(m, node);
                ret.arr_set(subidx, c);
                Some(ret)
            }
        }
    } else if subidx == 0 {
        None
    } else {
        Some(array_resized(m, node, subidx))
    }
}

/// Build a vector in one pass (no intermediate garbage).
pub fn vec_from_slice(m: &mut Mutator, xs: &[Value]) -> Value {
    let n = xs.len();
    if n == 0 {
        return vec_empty();
    }
    let toff = tailoff(n);
    let tail = array_from(m, &xs[toff..]);
    let mut nodes: Vec<Value> = xs[..toff].chunks(WIDTH).map(|c| array_from(m, c)).collect();
    let mut shift = BITS;
    while nodes.len() > WIDTH {
        nodes = nodes.chunks(WIDTH).map(|c| array_from(m, c)).collect();
        shift += BITS;
    }
    let root = array_from(m, &nodes);
    mk(m, n, shift, root, tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::init_for_tests;

    fn collect(v: Value) -> Vec<i64> {
        let mut out = vec![];
        v.vec_for_each(|x| {
            out.push(x.as_fixnum());
            true
        });
        out
    }

    #[test]
    fn conj_nth_assoc_pop_agree_with_vec() {
        let mut m = init_for_tests();
        let mut v = vec_empty();
        let n = 40_000;
        for i in 0..n {
            v = vec_conj(&mut m, v, Value::fix(i));
        }
        assert_eq!(v.vec_count(), n as usize);
        for i in (0..n).step_by(97) {
            assert_eq!(v.vec_nth(i as usize).as_fixnum(), i);
        }
        assert_eq!(collect(v), (0..n).collect::<Vec<_>>());

        let w = vec_assoc(&mut m, v, 12345, Value::fix(-1));
        assert_eq!(w.vec_nth(12345).as_fixnum(), -1);
        assert_eq!(v.vec_nth(12345).as_fixnum(), 12345, "original unchanged");

        let mut p = v;
        for i in (0..n).rev() {
            assert_eq!(p.vec_nth(p.vec_count() - 1).as_fixnum(), i);
            p = vec_pop(&mut m, p);
        }
        assert_eq!(p.vec_count(), 0);
    }

    #[test]
    fn bulk_build_matches_conj() {
        let mut m = init_for_tests();
        for n in [0usize, 1, 31, 32, 33, 1024, 1056, 1057, 33_000] {
            let xs: Vec<Value> = (0..n as i64).map(Value::fix).collect();
            let a = vec_from_slice(&mut m, &xs);
            let mut b = vec_empty();
            for &x in &xs {
                b = vec_conj(&mut m, b, x);
            }
            assert_eq!(collect(a), collect(b), "n={n}");
            // Continue conj-ing onto the bulk-built one to check its shape.
            let a2 = vec_conj(&mut m, a, Value::fix(-7));
            assert_eq!(a2.vec_nth(n).as_fixnum(), -7);
        }
    }
}
