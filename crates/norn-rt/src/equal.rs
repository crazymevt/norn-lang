//! Structural equality and hashing (runtime.md §2.3).
//!
//! - Numbers: `=` distinguishes integers from floats (Clojure semantics);
//!   numeric `==` lives in the primitives.
//! - Sequential collections (lists, vectors, ranges, seqs) are equal when
//!   their elements are equal, regardless of concrete type, and hash alike.
//! - Hashes are deterministic (no per-process seed) so images are reproducible.

use crate::object::Kind;
use crate::seq::SeqIter;
use crate::value::Value;

/// splitmix64 finaliser.
#[inline]
pub fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// FNV-1a over UTF-8 bytes.
pub fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

const SEQ_SEED: u64 = 0x5eed_0001;
const MAP_SEED: u64 = 0x5eed_0002;
const SET_SEED: u64 = 0x5eed_0003;

pub fn hash(v: Value) -> u64 {
    if !v.is_obj() && !v.is_cons() {
        return mix64(v.0);
    }
    if v.is_cons() {
        return hash_sequential(v);
    }
    match v.obj_kind() {
        Kind::String => mix64(hash_str(v.as_str())),
        Kind::Bytes => {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ 0xb7e5;
            for &b in v.as_bytes() {
                h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
            }
            mix64(h)
        }
        Kind::Symbol | Kind::Keyword => v.sym_hash(),
        Kind::Float => {
            let f = v.as_f64();
            // -0.0 = 0.0, and every NaN = every NaN (design §3.4).
            let f = if f == 0.0 { 0.0 } else if f.is_nan() { f64::NAN } else { f };
            mix64(f.to_bits() ^ 0xf10a7)
        }
        Kind::BigInt => crate::numbers::big_hash(v),
        Kind::Ratio => mix64(hash(v.field(0)) ^ hash(v.field(1)).rotate_left(23) ^ 0x7a710),
        Kind::Vector | Kind::VecSeq | Kind::Range | Kind::EmptyList | Kind::Stream => hash_sequential(v),
        Kind::Map => {
            let mut h = MAP_SEED;
            v.map_for_each(|k, val| {
                h = h.wrapping_add(hash(k) ^ mix64(hash(val)));
                true
            });
            mix64(h)
        }
        // The same order-independent formulas as hash maps and sets.
        Kind::Sorted => {
            let set = v.sorted_is_set();
            let mut h = if set { SET_SEED } else { MAP_SEED };
            crate::colls::sorted_each(v.sorted_root(), &mut |k, val| {
                h = h.wrapping_add(if set { hash(k) } else { hash(k) ^ mix64(hash(val)) });
                true
            });
            mix64(h)
        }
        Kind::Queue => {
            let mut h = SEQ_SEED;
            for x in crate::colls::queue_items(v) {
                h = h.wrapping_mul(31).wrapping_add(hash(x));
            }
            mix64(h)
        }
        Kind::Set => {
            let mut h = SET_SEED;
            v.set_for_each(|k| {
                h = h.wrapping_add(hash(k));
                true
            });
            mix64(h)
        }
        Kind::Instance => {
            let class = v.instance_class();
            if crate::clos::policy_flags(class) & crate::clos::class::IDENTITY != 0 {
                return v.identity_hash();
            }
            let mut h = class.identity_hash();
            for (k, x) in compared_slots(v) {
                let _ = k;
                h = h.wrapping_mul(31).wrapping_add(hash(x));
            }
            mix64(h)
        }
        _ => v.identity_hash(),
    }
}

fn hash_sequential(v: Value) -> u64 {
    let mut h = SEQ_SEED;
    if let Some(it) = SeqIter::new(v) {
        for x in it {
            h = h.wrapping_mul(31).wrapping_add(hash(x));
        }
    }
    mix64(h)
}

pub fn is_sequential(v: Value) -> bool {
    v.is_cons() || (v.is_obj() && matches!(v.obj_kind(), Kind::Vector | Kind::VecSeq | Kind::Range | Kind::EmptyList | Kind::Stream | Kind::Queue))
}

fn is_map_like(v: Value) -> bool {
    v.is(Kind::Map) || (v.is(Kind::Sorted) && !v.sorted_is_set())
}

fn is_set_like(v: Value) -> bool {
    v.is(Kind::Set) || (v.is(Kind::Sorted) && v.sorted_is_set())
}

fn map_like_count(v: Value) -> usize {
    if v.is(Kind::Map) { v.map_count() } else { v.sorted_count() }
}

fn set_like_count(v: Value) -> usize {
    if v.is(Kind::Set) { v.set_count() } else { v.sorted_count() }
}

/// A sorted collection equals a hash map or set with the same entries (and
/// two sorted ones with equal entries are equal: they list them in the
/// same order).
fn sorted_equal(a: Value, b: Value) -> bool {
    let (s, other) = if a.is(Kind::Sorted) { (a, b) } else { (b, a) };
    if other.is(Kind::Sorted) {
        let (x, y) = (crate::colls::sorted_entries(s), crate::colls::sorted_entries(other));
        return x.len() == y.len() && x.iter().zip(&y).all(|(p, q)| equal(p.0, q.0) && equal(p.1, q.1));
    }
    crate::colls::sorted_each(s.sorted_root(), &mut |k, v| {
        if other.is(Kind::Map) { matches!(other.map_get(k), Some(w) if equal(v, w)) } else { other.set_contains(k) }
    })
}

/// The slots `=` and `hash` look at: the current layout's, less any marked
/// `:compare false` (design §3.4).
fn compared_slots(inst: Value) -> Vec<(Value, Value)> {
    let class = inst.instance_class();
    let layout = class.class_field(crate::clos::class::LAYOUT);
    crate::clos::layout_keys(layout)
        .iter()
        .filter(|&&k| !crate::clos::slot_excluded(class, k))
        .map(|&k| (k, crate::clos::slot_get(inst, k).unwrap_or(Value::NIL)))
        .collect()
}

pub fn equal(a: Value, b: Value) -> bool {
    if a == b {
        return true;
    }
    if is_map_like(a) && is_map_like(b) && (a.is(Kind::Sorted) || b.is(Kind::Sorted)) {
        return map_like_count(a) == map_like_count(b) && sorted_equal(a, b);
    }
    if is_set_like(a) && is_set_like(b) && (a.is(Kind::Sorted) || b.is(Kind::Sorted)) {
        return set_like_count(a) == set_like_count(b) && sorted_equal(a, b);
    }
    if a.is(Kind::Queue) || b.is(Kind::Queue) {
        let items = |v: Value| if v.is(Kind::Queue) { Some(crate::colls::queue_items(v)) } else { SeqIter::new(v).map(|it| it.collect()) };
        return match (items(a), items(b)) {
            (Some(x), Some(y)) => x.len() == y.len() && x.iter().zip(&y).all(|(&p, &q)| equal(p, q)),
            _ => false,
        };
    }
    if is_sequential(a) {
        if !is_sequential(b) {
            return false;
        }
        let (Some(mut ia), Some(mut ib)) = (SeqIter::new(a), SeqIter::new(b)) else {
            return false;
        };
        loop {
            match (ia.next(), ib.next()) {
                (None, None) => return true,
                (Some(x), Some(y)) if equal(x, y) => {}
                _ => return false,
            }
        }
    }
    if !a.is_obj() || !b.is_obj() || a.obj_kind() != b.obj_kind() {
        return false;
    }
    match a.obj_kind() {
        Kind::String => a.as_str() == b.as_str(),
        Kind::Bytes => a.as_bytes() == b.as_bytes(),
        // `=` is an equivalence: NaN equals NaN (`==` keeps IEEE).
        Kind::Float => {
            let (x, y) = (a.as_f64(), b.as_f64());
            x == y || (x.is_nan() && y.is_nan())
        }
        Kind::BigInt => crate::numbers::big_equal(a, b),
        Kind::Ratio => equal(a.field(0), b.field(0)) && equal(a.field(1), b.field(1)),
        Kind::Map => {
            a.map_count() == b.map_count()
                && a.map_for_each(|k, v| matches!(b.map_get(k), Some(w) if equal(v, w)))
        }
        Kind::Set => a.set_count() == b.set_count() && a.set_for_each(|k| b.set_contains(k)),
        Kind::Instance => {
            let class = a.instance_class();
            class == b.instance_class()
                && crate::clos::policy_flags(class) & crate::clos::class::IDENTITY == 0
                && {
                    let (x, y) = (compared_slots(a), compared_slots(b));
                    x.iter().zip(&y).all(|(&(_, p), &(_, q))| equal(p, q))
                }
        }
        _ => false,
    }
}
