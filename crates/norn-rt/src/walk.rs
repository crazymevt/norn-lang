//! Walking the live heap from the GC roots (fiber dumps, diagnostics).
//!
//! The caller must have stopped every fiber (as an image save does): the
//! walk reads objects without synchronisation.

use crate::conc::fiber;
use crate::func::SEG_HEADER;
use crate::heap::{kind_at, scan};
use crate::object::Kind;
use crate::value::Value;
use std::collections::HashSet;

fn is_ref(v: Value) -> bool {
    v.is_obj() || v.is_cons()
}

/// Every value `v` holds strongly, the way the GC sees it: a fiber's stack
/// through its segments (live frames only), a weak reference's cleanup but
/// not its referent.
pub fn for_each_ref(v: Value, f: &mut dyn FnMut(Value)) {
    let p = v.ptr();
    // SAFETY: offsets come from the object's own layout.
    let word = |i: usize| Value(unsafe { *p.add(i) });
    match kind_at(p) {
        Kind::Fiber => fiber::VALUE_FIELDS.iter().for_each(|&i| f(word(2 + i))),
        Kind::Segment => {
            f(word(3));
            (0..v.seg_live_words()).for_each(|i| f(word(2 + SEG_HEADER + i)));
        }
        _ => scan(p, &mut |slot| f(word((slot.0.as_usize() - p as usize) / 8))),
    }
}

/// Call `on` once for every object reachable from `roots`.
pub fn reachable(roots: &[Value], mut on: impl FnMut(Value)) {
    let mut seen: HashSet<u64> = HashSet::new();
    let mut todo: Vec<Value> = roots.iter().copied().filter(|&r| is_ref(r)).collect();
    while let Some(v) = todo.pop() {
        if !seen.insert(v.ptr() as u64) {
            continue;
        }
        on(v);
        for_each_ref(v, &mut |x| {
            if is_ref(x) && !seen.contains(&(x.ptr() as u64)) {
                todo.push(x);
            }
        });
    }
}

/// Every GC root: the runtime tables, image objects, and the VM's roots.
pub fn gc_roots() -> Vec<Value> {
    let mut out = Vec::new();
    crate::runtime::for_each_root(&mut |v| out.push(v));
    crate::image::for_each_root(&mut |v| out.push(v));
    crate::heap::for_each_extra_root(&mut |v| out.push(v));
    out
}
