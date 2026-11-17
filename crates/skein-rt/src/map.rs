//! Persistent hash map and set: a hash array mapped trie (HAMT).
//!
//! ```text
//! Map:           [count][root: MapNode | CollisionNode | nil]
//! MapNode:       [bitmap][npairs][k0 v0 k1 v1 …]   k == NODE ⇒ v is a child node
//! CollisionNode: [hash][npairs][k0 v0 …]          all keys share one 64-bit hash
//! Set:           [map]   (values are `true`)
//! ```
//!
//! Hashing and equality are structural and defined by the runtime
//! (runtime.md §2.3), so no user code ever runs inside a map operation.

use crate::equal::{equal, hash};
use crate::heap::Mutator;
use crate::object::{Kind, alloc};
use crate::runtime::rt;
use crate::value::Value;

const BITS: u32 = 5;
const MASK: u64 = 31;

// ---- nodes -------------------------------------------------------------

fn node_alloc(m: &mut Mutator, kind: Kind, w0: u64, npairs: usize) -> Value {
    let n = alloc(m, kind, 2 + 2 * npairs);
    n.set_word(0, w0);
    n.set_word(1, npairs as u64);
    n
}

#[inline]
fn npairs(n: Value) -> usize {
    n.word(1) as usize
}
#[inline]
fn key_at(n: Value, i: usize) -> Value {
    n.field(2 + 2 * i)
}
#[inline]
fn val_at(n: Value, i: usize) -> Value {
    n.field(3 + 2 * i)
}

fn copy_node(m: &mut Mutator, n: Value) -> Value {
    let c = node_alloc(m, n.obj_kind(), n.word(0), npairs(n));
    for i in 0..2 * npairs(n) {
        c.set_field(2 + i, n.field(2 + i));
    }
    c
}

fn with_val(m: &mut Mutator, n: Value, i: usize, v: Value) -> Value {
    let c = copy_node(m, n);
    c.set_field(3 + 2 * i, v);
    c
}

fn with_pair(m: &mut Mutator, n: Value, i: usize, k: Value, v: Value) -> Value {
    let c = copy_node(m, n);
    c.set_field(2 + 2 * i, k);
    c.set_field(3 + 2 * i, v);
    c
}

/// Copy with a new pair inserted at index `at`.
fn insert_pair(m: &mut Mutator, n: Value, w0: u64, at: usize, k: Value, v: Value) -> Value {
    let np = npairs(n);
    let c = node_alloc(m, n.obj_kind(), w0, np + 1);
    let mut j = 0;
    for i in 0..=np {
        if i == at {
            c.set_field(2 + 2 * i, k);
            c.set_field(3 + 2 * i, v);
        } else {
            c.set_field(2 + 2 * i, key_at(n, j));
            c.set_field(3 + 2 * i, val_at(n, j));
            j += 1;
        }
    }
    c
}

/// Copy with pair `at` removed.
fn remove_pair(m: &mut Mutator, n: Value, w0: u64, at: usize) -> Value {
    let np = npairs(n);
    let c = node_alloc(m, n.obj_kind(), w0, np - 1);
    let mut j = 0;
    for i in 0..np {
        if i != at {
            c.set_field(2 + 2 * j, key_at(n, i));
            c.set_field(3 + 2 * j, val_at(n, i));
            j += 1;
        }
    }
    c
}

#[inline]
fn bit_for(h: u64, shift: u32) -> u64 {
    1u64 << ((h >> shift) & MASK)
}

fn node_get(mut node: Value, mut shift: u32, h: u64, key: Value) -> Option<Value> {
    loop {
        if node.is(Kind::CollisionNode) {
            return (0..npairs(node)).find(|&i| equal(key_at(node, i), key)).map(|i| val_at(node, i));
        }
        let bitmap = node.word(0);
        let bit = bit_for(h, shift);
        if bitmap & bit == 0 {
            return None;
        }
        let pos = (bitmap & (bit - 1)).count_ones() as usize;
        let k = key_at(node, pos);
        if k == Value::NODE {
            node = val_at(node, pos);
            shift += BITS;
            continue;
        }
        return if equal(k, key) { Some(val_at(node, pos)) } else { None };
    }
}

fn node_assoc(m: &mut Mutator, node: Value, shift: u32, h: u64, key: Value, val: Value, added: &mut bool) -> Value {
    if node.is(Kind::CollisionNode) {
        let ch = node.word(0);
        if ch == h {
            for i in 0..npairs(node) {
                if equal(key_at(node, i), key) {
                    return if val_at(node, i) == val { node } else { with_val(m, node, i, val) };
                }
            }
            *added = true;
            return insert_pair(m, node, ch, npairs(node), key, val);
        }
        // Different hash: lift the collision node into a bitmap node here.
        let wrapper = node_alloc(m, Kind::MapNode, bit_for(ch, shift), 1);
        wrapper.set_field(2, Value::NODE);
        wrapper.set_field(3, node);
        return node_assoc(m, wrapper, shift, h, key, val, added);
    }

    let bitmap = node.word(0);
    let bit = bit_for(h, shift);
    let pos = (bitmap & (bit - 1)).count_ones() as usize;
    if bitmap & bit == 0 {
        *added = true;
        return insert_pair(m, node, bitmap | bit, pos, key, val);
    }
    let k = key_at(node, pos);
    let v = val_at(node, pos);
    if k == Value::NODE {
        let child = node_assoc(m, v, shift + BITS, h, key, val, added);
        return if child == v { node } else { with_val(m, node, pos, child) };
    }
    if equal(k, key) {
        return if v == val { node } else { with_val(m, node, pos, val) };
    }
    *added = true;
    let sub = create_node(m, shift + BITS, k, v, h, key, val);
    with_pair(m, node, pos, Value::NODE, sub)
}

fn create_node(m: &mut Mutator, shift: u32, k1: Value, v1: Value, h2: u64, k2: Value, v2: Value) -> Value {
    let h1 = hash(k1);
    if h1 == h2 {
        let c = node_alloc(m, Kind::CollisionNode, h1, 2);
        c.set_field(2, k1);
        c.set_field(3, v1);
        c.set_field(4, k2);
        c.set_field(5, v2);
        return c;
    }
    let empty = node_alloc(m, Kind::MapNode, 0, 0);
    let mut added = false;
    let n = node_assoc(m, empty, shift, h1, k1, v1, &mut added);
    node_assoc(m, n, shift, h2, k2, v2, &mut added)
}

enum Del {
    Same,
    Gone,
    New(Value),
}

fn node_dissoc(m: &mut Mutator, node: Value, shift: u32, h: u64, key: Value) -> Del {
    if node.is(Kind::CollisionNode) {
        return match (0..npairs(node)).find(|&i| equal(key_at(node, i), key)) {
            None => Del::Same,
            Some(_) if npairs(node) == 1 => Del::Gone,
            Some(i) => Del::New(remove_pair(m, node, node.word(0), i)),
        };
    }
    let bitmap = node.word(0);
    let bit = bit_for(h, shift);
    if bitmap & bit == 0 {
        return Del::Same;
    }
    let pos = (bitmap & (bit - 1)).count_ones() as usize;
    let k = key_at(node, pos);
    let remove = |m: &mut Mutator| {
        if npairs(node) == 1 { Del::Gone } else { Del::New(remove_pair(m, node, bitmap & !bit, pos)) }
    };
    if k == Value::NODE {
        return match node_dissoc(m, val_at(node, pos), shift + BITS, h, key) {
            Del::Same => Del::Same,
            Del::New(c) => Del::New(with_val(m, node, pos, c)),
            Del::Gone => remove(m),
        };
    }
    if equal(k, key) { remove(m) } else { Del::Same }
}

fn node_for_each(node: Value, f: &mut dyn FnMut(Value, Value) -> bool) -> bool {
    for i in 0..npairs(node) {
        let k = key_at(node, i);
        let v = val_at(node, i);
        let go_on = if k == Value::NODE && !node.is(Kind::CollisionNode) { node_for_each(v, f) } else { f(k, v) };
        if !go_on {
            return false;
        }
    }
    true
}

// ---- maps --------------------------------------------------------------

pub(crate) fn map_make(m: &mut Mutator, count: usize, root: Value) -> Value {
    let map = alloc(m, Kind::Map, 2);
    map.set_word(0, count as u64);
    map.set_field(1, root);
    map
}

pub fn map_empty() -> Value {
    rt().empty_map
}

impl Value {
    #[inline]
    pub fn map_count(self) -> usize {
        self.word(0) as usize
    }

    pub fn map_get(self, key: Value) -> Option<Value> {
        let root = self.field(1);
        if root.is_nil() { None } else { node_get(root, 0, hash(key), key) }
    }

    /// Visit entries; stop early when `f` returns false.
    pub fn map_for_each(self, mut f: impl FnMut(Value, Value) -> bool) -> bool {
        let root = self.field(1);
        root.is_nil() || node_for_each(root, &mut f)
    }
}

pub fn map_assoc(m: &mut Mutator, map: Value, key: Value, val: Value) -> Value {
    let root = map.field(1);
    let root = if root.is_nil() { node_alloc(m, Kind::MapNode, 0, 0) } else { root };
    let mut added = false;
    let new_root = node_assoc(m, root, 0, hash(key), key, val, &mut added);
    if new_root == map.field(1) {
        return map;
    }
    map_make(m, map.map_count() + added as usize, new_root)
}

pub fn map_dissoc(m: &mut Mutator, map: Value, key: Value) -> Value {
    let root = map.field(1);
    if root.is_nil() {
        return map;
    }
    match node_dissoc(m, root, 0, hash(key), key) {
        Del::Same => map,
        Del::Gone => map_empty(),
        Del::New(r) => map_make(m, map.map_count() - 1, r),
    }
}

// ---- sets --------------------------------------------------------------

pub(crate) fn set_make(m: &mut Mutator, map: Value) -> Value {
    let s = alloc(m, Kind::Set, 1);
    s.set_field(0, map);
    s
}

pub fn set_empty() -> Value {
    rt().empty_set
}

impl Value {
    #[inline]
    pub fn set_map(self) -> Value {
        self.field(0)
    }

    pub fn set_count(self) -> usize {
        self.set_map().map_count()
    }

    pub fn set_contains(self, x: Value) -> bool {
        self.set_map().map_get(x).is_some()
    }

    pub fn set_for_each(self, mut f: impl FnMut(Value) -> bool) -> bool {
        self.set_map().map_for_each(|k, _| f(k))
    }
}

pub fn set_conj(m: &mut Mutator, s: Value, x: Value) -> Value {
    let map = map_assoc(m, s.set_map(), x, Value::TRUE);
    if map == s.set_map() { s } else { set_make(m, map) }
}

pub fn set_disj(m: &mut Mutator, s: Value, x: Value) -> Value {
    let map = map_dissoc(m, s.set_map(), x);
    if map == s.set_map() { s } else { set_make(m, map) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::init_for_tests;
    use crate::string::str_new;

    #[test]
    fn assoc_get_dissoc_many() {
        let mut m = init_for_tests();
        let mut map = map_empty();
        let n = 20_000;
        for i in 0..n {
            map = map_assoc(&mut m, map, Value::fix(i), Value::fix(i * 2));
        }
        assert_eq!(map.map_count(), n as usize);
        for i in 0..n {
            assert_eq!(map.map_get(Value::fix(i)), Some(Value::fix(i * 2)));
        }
        assert_eq!(map.map_get(Value::fix(n)), None);
        // Overwrite doesn't change count.
        let map2 = map_assoc(&mut m, map, Value::fix(5), Value::fix(0));
        assert_eq!(map2.map_count(), map.map_count());
        assert_eq!(map.map_get(Value::fix(5)), Some(Value::fix(10)), "original unchanged");
        // Remove everything.
        let mut d = map;
        for i in 0..n {
            d = map_dissoc(&mut m, d, Value::fix(i));
            assert_eq!(d.map_count(), (n - i - 1) as usize);
        }
        assert!(d.map_get(Value::fix(1)).is_none());
    }

    #[test]
    fn structural_string_keys() {
        let mut m = init_for_tests();
        let a = str_new(&mut m, "hello");
        let b = str_new(&mut m, "hello");
        let map = map_assoc(&mut m, map_empty(), a, Value::fix(1));
        assert_eq!(map.map_get(b), Some(Value::fix(1)));
        let mut seen = 0;
        map.map_for_each(|_, _| {
            seen += 1;
            true
        });
        assert_eq!(seen, 1);
    }
}
