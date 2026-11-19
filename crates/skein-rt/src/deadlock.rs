//! Deadlock detection: parked fibers nothing can wake (runtime.md §13,
//! question 4; docs/plans/plan-deadlock.md).
//!
//! Whatever can wake a parked fiber holds its waiter, and so the fiber: a
//! channel's queue, a fiber it awaits, a scope it joins, or, outside the
//! heap, a GC root (timers, the reactor, the blocking pool, native threads).
//! So a parked fiber the collector didn't reach can never be woken. Instead
//! of freeing it, the collection keeps it (and all it reaches), and a
//! carrier then wakes it with a `Deadlock` condition (`take_ready`).
//!
//! Every fiber is registered when made, as weak references are, untraced;
//! each collection drops the ones that ended or were freed. Only a full
//! collection can tell: fibers stay where they're made (`Kind::stays_put`),
//! in the non-moving space, which a young collection counts as reached.

use crate::conc::fiber;
use crate::heap::Skein;
use crate::value::Value;
use crate::weak::{moved, object, reachable};
use mmtk::scheduler::GCWorker;
use mmtk::vm::{ObjectTracer, ObjectTracerContext};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Every fiber not known to have ended (untraced), in shards by id, so
/// carriers spawning at once don't wait for one lock.
const SHARDS: usize = 16;
static REGISTRY: [Mutex<Vec<Value>>; SHARDS] = [const { Mutex::new(Vec::new()) }; SHARDS];
/// Deadlocked fibers kept by the last collection, for a carrier to wake.
static READY: Mutex<Vec<Value>> = Mutex::new(Vec::new());
static ANY_READY: AtomicBool = AtomicBool::new(false);

/// Track fiber `f` (made, or brought back from an image).
pub fn register(f: Value) {
    REGISTRY[f.fiber_id() as usize % SHARDS].lock().unwrap().push(f);
}

/// GC roots: the deadlocked fibers not yet woken.
pub fn for_each_root(f: &mut dyn FnMut(Value)) {
    READY.lock().unwrap().iter().for_each(|&v| f(v));
}

/// At a collection's weak pass, before weak references are cleared (world
/// stopped): keep each parked fiber nothing reached, and list it to be
/// woken. True if it kept any (the trace then runs again, and so does this,
/// finding them reached).
pub fn process(worker: &mut GCWorker<Skein>, tracer: &impl ObjectTracerContext<Skein>) -> bool {
    // (a young collection counts every fiber reached, so finds nothing; and
    // the registry is as long as the fibers alive, so it waits for a full
    // one, ended fibers' entries with it)
    if crate::heap::in_young_collection() {
        return false;
    }
    let mut stuck = Vec::new();
    for shard in &REGISTRY {
        let mut reg = shard.lock().unwrap();
        reg.retain_mut(|f| {
            let state = f.fiber_state().load(Ordering::Acquire);
            if reachable(*f) {
                if state == fiber::DONE {
                    return false;
                }
                // (fibers stay put; a moving collector would have moved it)
                if let Some(to) = moved(*f) {
                    *f = to;
                }
                return true;
            }
            // (unreached and not parked: it ended, and is freed with it)
            if state != fiber::PARKED {
                return false;
            }
            stuck.push(*f);
            true
        });
    }
    if stuck.is_empty() {
        return false;
    }
    let kept: Vec<Value> = tracer.with_tracer(worker, |t| {
        stuck
            .iter()
            .filter_map(|&f| {
                let r = t.trace_object(object(f)?);
                Some(Value(r.to_raw_address().as_usize() as u64 | (f.0 & crate::value::TAG_MASK)))
            })
            .collect()
    });
    for (&from, &to) in stuck.iter().zip(&kept) {
        if from != to {
            let mut reg = REGISTRY[from.fiber_id() as usize % SHARDS].lock().unwrap();
            if let Some(slot) = reg.iter_mut().find(|v| **v == from) {
                *slot = to;
            }
        }
    }
    READY.lock().unwrap().extend(kept);
    ANY_READY.store(true, Ordering::Release);
    true
}

/// The deadlocked fibers the last collections kept, to be woken (once each).
pub fn take_ready() -> Vec<Value> {
    if !ANY_READY.swap(false, Ordering::AcqRel) {
        return Vec::new();
    }
    std::mem::take(&mut *READY.lock().unwrap())
}

/// How many fibers are registered (for tests and `gc-stats`).
pub fn registered() -> usize {
    REGISTRY.iter().map(|r| r.lock().unwrap().len()).sum()
}
