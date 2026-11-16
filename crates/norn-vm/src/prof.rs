//! The sampling profiler (M9b part 2; `norn.prof` is its interface).
//!
//! While a profile runs, a timer thread wakes every interval, bumps the
//! sample epoch and the safepoint attention word. Each carrier then takes
//! its next poll's slow path (function entry or a loop's back edge, at
//! either tier: tier 1 bails to the interpreter when attention changes) and
//! records the stack of the fiber it's running, its own: no thread reads a
//! stack it doesn't own. An idle carrier, or one in a native call, records
//! nothing, so a profile counts time spent running Norn code. Stacks are
//! kept as the names of their frames' code, counted.

use crate::vm::{Error, Result, walk_frames};
use norn_rt::func::F_PROTO;
use norn_rt::{Kind, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Is a profile running?
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Bumped once per interval; a carrier samples once per value it sees.
static EPOCH: AtomicU64 = AtomicU64::new(0);
static PROFILE: Mutex<Option<Profile>> = Mutex::new(None);

/// A stack this deep is cut, keeping its innermost frames.
const MAX_DEPTH: usize = 512;

pub(crate) struct Profile {
    interval_us: u64,
    started: Instant,
    samples: u64,
    /// Frame names, numbered.
    names: Vec<String>,
    /// A name symbol's address (interned symbols live as long as the
    /// process) → its number.
    by_symbol: HashMap<u64, u32>,
    /// Stacks, outermost frame first, → how many samples saw them.
    stacks: HashMap<Vec<u32>, u64>,
}

impl Profile {
    fn name_id(&mut self, name: Value) -> u32 {
        if name.is_symbol() {
            if let Some(&id) = self.by_symbol.get(&name.0) {
                return id;
            }
            let id = self.intern(norn_rt::print::pr_str(name, false));
            self.by_symbol.insert(name.0, id);
            id
        } else {
            self.intern("fn".to_owned())
        }
    }

    fn intern(&mut self, s: String) -> u32 {
        if let Some(i) = self.names.iter().position(|n| *n == s) {
            return i as u32;
        }
        self.names.push(s);
        (self.names.len() - 1) as u32
    }
}

/// Is a sample due on this carrier? (`seen` is its last epoch.) Cheap: only
/// asked on a poll's slow path.
#[inline]
pub(crate) fn due(seen: &mut u64) -> bool {
    if !ACTIVE.load(Ordering::Relaxed) {
        return false;
    }
    let e = EPOCH.load(Ordering::Acquire);
    if e == *seen {
        return false;
    }
    *seen = e;
    true
}

/// Record the stack whose innermost frame is (seg, fp); when `entry`, that
/// frame's function has just been entered, and the sample is its caller's
/// (a poll comes at a function's entry or a loop's turn, so time spent in
/// native calls since the last one shows where it was spent, not in the
/// next function called).
pub(crate) fn sample(seg: Value, fp: usize, entry: bool) {
    if seg.is_nil() {
        return;
    }
    let mut frames = walk_frames(seg, fp);
    if entry && frames.len() > 1 {
        frames.remove(0);
    }
    let mut guard = PROFILE.lock().unwrap();
    let Some(p) = guard.as_mut() else { return };
    let take = frames.len().min(MAX_DEPTH);
    let mut stack = Vec::with_capacity(take);
    for &(s, f) in frames[..take].iter().rev() {
        // SAFETY: walk_frames yields live frames of the running fiber.
        let proto = unsafe { *s.seg_data().add(f + F_PROTO) };
        stack.push(p.name_id(if proto.is(Kind::Proto) { proto.proto_name() } else { Value::NIL }));
    }
    p.samples += 1;
    *p.stacks.entry(stack).or_insert(0) += 1;
}

/// `(%prof-start! interval-us)`: start a profile, sampling every carrier
/// about every interval (at least 50 µs).
pub(crate) fn start(interval_us: i64) -> Result<Value> {
    let interval_us = interval_us.max(50) as u64;
    if ACTIVE.swap(true, Ordering::AcqRel) {
        return Err(Error::new("A profile is running already (one at a time: norn.prof/stop! ends it)"));
    }
    *PROFILE.lock().unwrap() = Some(Profile {
        interval_us,
        started: Instant::now(),
        samples: 0,
        names: Vec::new(),
        by_symbol: HashMap::new(),
        stacks: HashMap::new(),
    });
    std::thread::Builder::new()
        .name("norn-profiler".into())
        .spawn(move || {
            while ACTIVE.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_micros(interval_us));
                EPOCH.fetch_add(1, Ordering::AcqRel);
                norn_rt::safepoint::bump_attention();
            }
        })
        .map_err(|e| {
            ACTIVE.store(false, Ordering::Release);
            Error::new(format!("can't start the profiler: {e}"))
        })?;
    Ok(Value::NIL)
}

/// `(%prof-stop!)`: end the profile: `[interval-us elapsed-us samples names
/// stacks]`, names a vector of strings and stacks a vector of `[[name-index
/// …] count]`, outermost frame first. nil when none was running.
pub(crate) fn stop(m: &mut norn_rt::Mutator) -> Value {
    if !ACTIVE.swap(false, Ordering::AcqRel) {
        return Value::NIL;
    }
    let Some(p) = PROFILE.lock().unwrap().take() else { return Value::NIL };
    use norn_rt::string::str_new;
    use norn_rt::vector::vec_from_slice;
    let names: Vec<Value> = p.names.iter().map(|n| str_new(m, n)).collect();
    let names = vec_from_slice(m, &names);
    let mut stacks = Vec::with_capacity(p.stacks.len());
    for (stack, n) in &p.stacks {
        let ids: Vec<Value> = stack.iter().map(|&i| Value::fix(i as i64)).collect();
        let ids = vec_from_slice(m, &ids);
        stacks.push(vec_from_slice(m, &[ids, Value::fix(*n as i64)]));
    }
    let stacks = vec_from_slice(m, &stacks);
    let elapsed = p.started.elapsed().as_micros() as i64;
    vec_from_slice(m, &[Value::fix(p.interval_us as i64), Value::fix(elapsed), Value::fix(p.samples as i64), names, stacks])
}

/// `(%prof-active?)`
pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}
