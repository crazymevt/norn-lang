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

/// The hash of v. Collections hash from their elements' hashes, worked out
/// with a stack of frames of our own rather than by recursing, so a value
/// nested a million deep hashes without exhausting the native stack.
pub fn hash(v: Value) -> u64 {
    let mut stack = match Frame::open(v) {
        Ok(h) => return h,
        Err(frame) => vec![frame],
    };
    loop {
        let top = stack.last_mut().unwrap();
        match top.next_child() {
            Some(child) => match Frame::open(child) {
                Ok(h) => top.take(h),
                Err(frame) => stack.push(frame),
            },
            None => {
                let h = stack.pop().unwrap().finish();
                match stack.last_mut() {
                    Some(parent) => parent.take(h),
                    None => return h,
                }
            }
        }
    }
}

/// How a collection combines its children's hashes.
enum Combine {
    /// Ordered: h·31 + child, from SEQ_SEED.
    Seq,
    /// Unordered keys: sum of hashes.
    Set,
    /// Unordered pairs: sum of key ^ mix64(value), children alternating
    /// key and value.
    Map,
    /// An instance's non-default slots, key and value alternating: sum of
    /// mix64(key·31 + value), then the class's identity.
    Slots(u64),
    /// A ratio's numerator and denominator.
    Ratio,
}

/// A collection being hashed: its children still to do, and the hash so far.
struct Frame {
    combine: Combine,
    seq: Option<SeqIter>,
    children: std::vec::IntoIter<Value>,
    h: u64,
    /// The key's hash, waiting for its value (Map, Slots) or the
    /// numerator's (Ratio).
    pending: Option<u64>,
}

impl Frame {
    fn new(combine: Combine, h: u64, children: Vec<Value>) -> Frame {
        Frame { combine, seq: None, children: children.into_iter(), h, pending: None }
    }

    /// v's hash if it has no children to hash, else a frame for them.
    fn open(v: Value) -> Result<u64, Frame> {
        if !v.is_obj() && !v.is_cons() {
            return Ok(mix64(v.0));
        }
        if v.is_cons() {
            return Err(Frame::sequential(v));
        }
        match v.obj_kind() {
            Kind::String => Ok(mix64(hash_str(v.as_str()))),
            Kind::Regex => Ok(mix64(hash_str(v.field(0).as_str()) ^ 0x7e9e)),
            Kind::Bytes => {
                let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ 0xb7e5;
                for &b in v.as_bytes() {
                    h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
                }
                Ok(mix64(h))
            }
            Kind::Symbol | Kind::Keyword => Ok(v.sym_hash()),
            Kind::Float => {
                let f = v.as_f64();
                // -0.0 = 0.0, and every NaN = every NaN (design §3.4).
                let f = if f == 0.0 {
                    0.0
                } else if f.is_nan() {
                    f64::NAN
                } else {
                    f
                };
                Ok(mix64(f.to_bits() ^ 0xf10a7))
            }
            Kind::BigInt => Ok(crate::numbers::big_hash(v)),
            Kind::Ratio => Err(Frame::new(Combine::Ratio, 0, vec![v.field(0), v.field(1)])),
            Kind::Vector | Kind::VecSeq | Kind::Range | Kind::EmptyList | Kind::Stream => Err(Frame::sequential(v)),
            Kind::Map => {
                let mut kv = Vec::with_capacity(2 * v.map_count());
                v.map_for_each(|k, x| {
                    kv.push(k);
                    kv.push(x);
                    true
                });
                Err(Frame::new(Combine::Map, MAP_SEED, kv))
            }
            // The same order-independent formulas as hash maps and sets.
            Kind::Sorted => {
                let set = v.sorted_is_set();
                let mut children = Vec::new();
                crate::colls::sorted_each(v.sorted_root(), &mut |k, x| {
                    children.push(k);
                    if !set {
                        children.push(x);
                    }
                    true
                });
                Err(if set { Frame::new(Combine::Set, SET_SEED, children) } else { Frame::new(Combine::Map, MAP_SEED, children) })
            }
            Kind::Queue => Err(Frame::new(Combine::Seq, SEQ_SEED, crate::colls::queue_items(v))),
            Kind::Set => {
                let mut items = Vec::with_capacity(v.set_count());
                v.set_for_each(|k| {
                    items.push(k);
                    true
                });
                Err(Frame::new(Combine::Set, SET_SEED, items))
            }
            Kind::Instance => {
                let class = v.instance_class();
                if crate::clos::policy_flags(class) & crate::clos::class::IDENTITY != 0 {
                    return Ok(v.identity_hash());
                }
                // A slot holding its class's default adds nothing, and the
                // others add their key and value in any order. So redefining
                // the class to add a defaulted slot, or to reorder slots,
                // keeps existing instances' hashes: they stay findable in
                // the maps and sets they're keys of. Equal instances hold the
                // same values, so they still hash alike. (Changing a slot's
                // default changes the hash of instances holding the old or
                // new one.)
                let layout = class.class_field(crate::clos::class::LAYOUT);
                let keys = crate::clos::layout_keys(layout);
                let defaults = &layout.arr_slice()[keys.len()..];
                let mut kv = Vec::new();
                for (k, x) in compared_slots(v) {
                    let default = keys.iter().position(|&key| key == k).map(|i| defaults[i]);
                    if !default.is_some_and(|d| equal(x, d)) {
                        kv.push(k);
                        kv.push(x);
                    }
                }
                Err(Frame::new(Combine::Slots(class.identity_hash()), 0, kv))
            }
            _ => Ok(v.identity_hash()),
        }
    }

    /// A sequential value's frame: its items are read as they're needed.
    fn sequential(v: Value) -> Frame {
        let mut f = Frame::new(Combine::Seq, SEQ_SEED, Vec::new());
        f.seq = SeqIter::new(v);
        f
    }

    fn next_child(&mut self) -> Option<Value> {
        match &mut self.seq {
            Some(it) => it.next(),
            None => self.children.next(),
        }
    }

    fn take(&mut self, child: u64) {
        match self.combine {
            Combine::Seq => self.h = self.h.wrapping_mul(31).wrapping_add(child),
            Combine::Set => self.h = self.h.wrapping_add(child),
            Combine::Map => match self.pending.take() {
                None => self.pending = Some(child),
                Some(k) => self.h = self.h.wrapping_add(k ^ mix64(child)),
            },
            Combine::Slots(_) => match self.pending.take() {
                None => self.pending = Some(child),
                Some(k) => self.h = self.h.wrapping_add(mix64(k.wrapping_mul(31).wrapping_add(child))),
            },
            Combine::Ratio => match self.pending.take() {
                None => self.pending = Some(child),
                Some(n) => self.h = n ^ child.rotate_left(23),
            },
        }
    }

    fn finish(self) -> u64 {
        match self.combine {
            Combine::Slots(class) => mix64(self.h ^ class),
            Combine::Ratio => mix64(self.h ^ 0x7a710),
            _ => mix64(self.h),
        }
    }
}

pub fn is_sequential(v: Value) -> bool {
    v.is_cons()
        || (v.is_obj() && matches!(v.obj_kind(), Kind::Vector | Kind::VecSeq | Kind::Range | Kind::EmptyList | Kind::Stream | Kind::Queue))
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
fn sorted_equal(a: Value, b: Value, stack: &mut Vec<Pending>) -> bool {
    let (s, other) = if a.is(Kind::Sorted) { (a, b) } else { (b, a) };
    if other.is(Kind::Sorted) {
        let (x, y) = (crate::colls::sorted_entries(s), crate::colls::sorted_entries(other));
        if x.len() != y.len() {
            return false;
        }
        for (p, q) in x.into_iter().zip(y).rev() {
            stack.push(Pending::Pair(p.1, q.1));
            stack.push(Pending::Pair(p.0, q.0));
        }
        return true;
    }
    crate::colls::sorted_each(s.sorted_root(), &mut |k, v| {
        if other.is(Kind::Map) {
            match other.map_get(k) {
                Some(w) => {
                    stack.push(Pending::Pair(v, w));
                    true
                }
                None => false,
            }
        } else {
            other.set_contains(k)
        }
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

/// Structural equality. Pairs still to compare go on a stack of our own
/// rather than the native one, so values nested a million deep compare
/// without exhausting it; the first difference ends the comparison.
pub fn equal(a: Value, b: Value) -> bool {
    let mut stack = vec![Pending::Pair(a, b)];
    while let Some(p) = stack.pop() {
        let ok = match p {
            Pending::Pair(a, b) => equal_one(a, b, &mut stack),
            Pending::Seqs(mut x, mut y) => match (x.next(), y.next()) {
                (None, None) => true,
                (Some(p), Some(q)) => {
                    stack.push(Pending::Seqs(x, y));
                    stack.push(Pending::Pair(p, q));
                    true
                }
                _ => false,
            },
        };
        if !ok {
            return false;
        }
    }
    true
}

/// What's left to compare: two values, or the rest of two sequences.
enum Pending {
    Pair(Value, Value),
    Seqs(SeqIter, SeqIter),
}

/// Whether a and b can be equal, looking at them alone: false when they
/// can't, else true, with the parts that must also be equal pushed.
fn equal_one(a: Value, b: Value, stack: &mut Vec<Pending>) -> bool {
    if a == b {
        return true;
    }
    if is_map_like(a) && is_map_like(b) && (a.is(Kind::Sorted) || b.is(Kind::Sorted)) {
        return map_like_count(a) == map_like_count(b) && sorted_equal(a, b, stack);
    }
    if is_set_like(a) && is_set_like(b) && (a.is(Kind::Sorted) || b.is(Kind::Sorted)) {
        return set_like_count(a) == set_like_count(b) && sorted_equal(a, b, stack);
    }
    if a.is(Kind::Queue) || b.is(Kind::Queue) {
        let items = |v: Value| if v.is(Kind::Queue) { Some(crate::colls::queue_items(v)) } else { SeqIter::new(v).map(|it| it.collect()) };
        return match (items(a), items(b)) {
            (Some(x), Some(y)) if x.len() == y.len() => {
                stack.extend(x.into_iter().zip(y).map(|(p, q)| Pending::Pair(p, q)));
                true
            }
            _ => false,
        };
    }
    if is_sequential(a) {
        if !is_sequential(b) {
            return false;
        }
        let (Some(ia), Some(ib)) = (SeqIter::new(a), SeqIter::new(b)) else {
            return false;
        };
        stack.push(Pending::Seqs(ia, ib));
        return true;
    }
    if !a.is_obj() || !b.is_obj() || a.obj_kind() != b.obj_kind() {
        return false;
    }
    match a.obj_kind() {
        Kind::String => a.as_str() == b.as_str(),
        Kind::Regex => a.field(0).as_str() == b.field(0).as_str(),
        Kind::Bytes => a.as_bytes() == b.as_bytes(),
        // `=` is an equivalence: NaN equals NaN (`==` keeps IEEE).
        Kind::Float => {
            let (x, y) = (a.as_f64(), b.as_f64());
            x == y || (x.is_nan() && y.is_nan())
        }
        Kind::BigInt => crate::numbers::big_equal(a, b),
        Kind::Ratio => {
            stack.push(Pending::Pair(a.field(1), b.field(1)));
            stack.push(Pending::Pair(a.field(0), b.field(0)));
            true
        }
        Kind::Map => {
            a.map_count() == b.map_count()
                && a.map_for_each(|k, v| match b.map_get(k) {
                    Some(w) => {
                        stack.push(Pending::Pair(v, w));
                        true
                    }
                    None => false,
                })
        }
        Kind::Set => a.set_count() == b.set_count() && a.set_for_each(|k| b.set_contains(k)),
        Kind::Instance => {
            let class = a.instance_class();
            class == b.instance_class() && crate::clos::policy_flags(class) & crate::clos::class::IDENTITY == 0 && {
                let (x, y) = (compared_slots(a), compared_slots(b));
                stack.extend(x.into_iter().zip(y).rev().map(|((_, p), (_, q))| Pending::Pair(p, q)));
                true
            }
        }
        _ => false,
    }
}
