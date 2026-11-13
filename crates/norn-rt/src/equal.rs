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
            let f = if f == 0.0 { 0.0 } else { f }; // -0.0 == 0.0
            mix64(f.to_bits() ^ 0xf10a7)
        }
        Kind::Vector | Kind::VecSeq | Kind::Range | Kind::EmptyList => hash_sequential(v),
        Kind::Map => {
            let mut h = MAP_SEED;
            v.map_for_each(|k, val| {
                h = h.wrapping_add(hash(k) ^ mix64(hash(val)));
                true
            });
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
            let mut h = v.instance_class().identity_hash();
            for x in crate::clos::instance_values(v) {
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
    v.is_cons() || (v.is_obj() && matches!(v.obj_kind(), Kind::Vector | Kind::VecSeq | Kind::Range | Kind::EmptyList))
}

pub fn equal(a: Value, b: Value) -> bool {
    if a == b {
        return true;
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
        Kind::Float => a.as_f64() == b.as_f64(),
        Kind::Map => {
            a.map_count() == b.map_count()
                && a.map_for_each(|k, v| matches!(b.map_get(k), Some(w) if equal(v, w)))
        }
        Kind::Set => a.set_count() == b.set_count() && a.set_for_each(|k| b.set_contains(k)),
        Kind::Instance => {
            a.instance_class() == b.instance_class() && {
                let (x, y) = (crate::clos::instance_values(a), crate::clos::instance_values(b));
                x.iter().zip(&y).all(|(&p, &q)| equal(p, q))
            }
        }
        _ => false,
    }
}
