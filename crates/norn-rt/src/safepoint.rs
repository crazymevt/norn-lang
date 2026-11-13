//! The safepoint protocol (runtime.md §4.2).
//!
//! Threads that touch the Norn heap are *participants*. A participant is
//! either **running** (may read or write heap objects at any moment) or
//! **safe** (blocked or idle, promises not to touch the heap). Stopping the
//! world means: raise `STOP`, bump the attention counter so every running
//! participant takes its slow poll path, and wait until none is running.
//!
//! Running participants notice at their next safepoint poll, make their
//! stacks exactly describable (top of segment saved), and block in
//! `block_for_stop`. Becoming running again waits for the world to restart.
//!
//! The attention counter is also how cancellation and external jobs reach a
//! busy carrier: anything that needs a running carrier's attention bumps it.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

static ATTENTION: AtomicU64 = AtomicU64::new(0);
static STOP: AtomicBool = AtomicBool::new(false);

struct State {
    running: usize,
    stopped: bool,
    epoch: u64,
}

static STATE: Mutex<State> = Mutex::new(State { running: 0, stopped: false, epoch: 0 });
static CV: Condvar = Condvar::new();

/// Address of the attention counter, for native code (tier 1) to poll.
pub fn attention_ptr() -> *const u64 {
    ATTENTION.as_ptr() as *const u64
}

/// Current attention epoch. Pollers compare it with the last value they saw.
#[inline]
pub fn attention() -> u64 {
    ATTENTION.load(Ordering::Acquire)
}

/// Ask every running participant to take its slow poll path soon.
pub fn bump_attention() {
    ATTENTION.fetch_add(1, Ordering::AcqRel);
}

#[inline]
pub fn stop_requested() -> bool {
    STOP.load(Ordering::Acquire)
}

/// This thread becomes a running participant (waits out any stop).
pub fn register() {
    leave_safe();
}

pub fn unregister() {
    enter_safe();
}

/// About to block or idle: stop counting as running.
pub fn enter_safe() {
    let mut s = STATE.lock().unwrap();
    s.running -= 1;
    CV.notify_all();
}

/// Resume touching the heap; waits while the world is stopped.
pub fn leave_safe() {
    let mut s = STATE.lock().unwrap();
    while s.stopped {
        s = CV.wait(s).unwrap();
    }
    s.running += 1;
}

/// Called at a safepoint when `stop_requested()`: wait out the stop.
pub fn block_for_stop() {
    enter_safe();
    leave_safe();
}

/// Run `f` as a running participant (for non-carrier threads such as the
/// timer thread, which are normally safe).
pub fn while_running<R>(f: impl FnOnce() -> R) -> R {
    leave_safe();
    let r = f();
    enter_safe();
    r
}

/// Collector side: stop all running participants. Returns once none runs.
pub fn stop_world() {
    let mut s = STATE.lock().unwrap();
    s.stopped = true;
    STOP.store(true, Ordering::Release);
    bump_attention();
    while s.running > 0 {
        s = CV.wait(s).unwrap();
    }
}

/// Collector side: restart the world.
pub fn resume_world() {
    let mut s = STATE.lock().unwrap();
    s.stopped = false;
    s.epoch += 1;
    STOP.store(false, Ordering::Release);
    CV.notify_all();
}

/// Number of completed stop-the-world pauses.
pub fn epoch() -> u64 {
    STATE.lock().unwrap().epoch
}

/// Block (as a safe participant) until a pause that starts after `since`
/// has finished. Used when a thread requests a collection itself.
pub fn wait_for_pause_after(since: u64) {
    let mut s = STATE.lock().unwrap();
    s.running -= 1;
    CV.notify_all();
    while s.epoch <= since || s.stopped {
        s = CV.wait(s).unwrap();
    }
    s.running += 1;
}

// ---- GC stress -------------------------------------------------------------

fn stress_interval() -> u64 {
    static N: OnceLock<u64> = OnceLock::new();
    *N.get_or_init(|| std::env::var("NORN_GC_STRESS").ok().and_then(|s| s.parse().ok()).unwrap_or(0))
}

thread_local! {
    static STRESS_COUNT: Cell<u64> = const { Cell::new(0) };
}

/// With `NORN_GC_STRESS=N`, true on every N-th slow-path poll of a thread
/// (slow paths happen at least once per time slice).
pub fn stress_due() -> bool {
    let n = stress_interval();
    if n == 0 {
        return false;
    }
    STRESS_COUNT.with(|c| {
        let k = c.get() + 1;
        c.set(k);
        k % n == 0
    })
}

pub fn stress_enabled() -> bool {
    stress_interval() > 0
}

/// Collect now, from a safepoint. The caller has saved its stack.
pub fn collect(m: &mut crate::heap::Mutator) {
    crate::heap::collect(m);
}
