//! The M:N scheduler (runtime.md §5): fibers on a work-stealing pool of
//! carrier threads, one per core, plus a timer thread.
//!
//! **Fiber states.** `QUEUED → RUNNING → {PARKED, DONE}`, with `NOTIFIED` for
//! a wake that arrives while the fiber is still running (between registering
//! a wait and committing the park). The owner commits a park with a CAS
//! `RUNNING → PARKED`; if that fails because the state is `NOTIFIED`, the
//! fiber simply continues. So there are no lost wake-ups and no locks around
//! parking.
//!
//! **Claims.** Every wait is a heap `Waiter` with a `Claim`. Whoever CASes the
//! claim first (a channel partner, a timer, a canceller, a finishing fiber)
//! owns the wake-up and delivers the value. An `alts!` shares one claim
//! between its waiters, so exactly one of its channels fires.
//!
//! **Roots.** Run queues, timers, root fibers and each carrier's current fiber
//! are the GC roots that live outside the heap (`for_each_root`).
//!
//! **Native regions and spare carriers.** A carrier inside a long native call
//! (an FFI call, a heavy primitive) counts as safe for the GC (`enter_native`).
//! The monitor thread watches for carriers stuck there while work is queued,
//! and starts a spare carrier in one of the extra slots (`n..2n`). A spare
//! retires once it's idle and enough regular carriers are back.
//!
//! **Pausing.** Saving an image needs every fiber stopped at a resume point.
//! `pause_all` sets `paused` and waits until no carrier is doing work: a
//! carrier counts itself `in_flight` before it picks a fiber or handles
//! external jobs, then checks `paused` (both sequentially consistent, so
//! either the carrier sees the pause or the pauser sees the carrier), and
//! running fibers yield at their next poll.

use crate::vm::{Error, Exit, Vm, cancel_error};
use skein_rt::conc::{fiber, fiber_new, is_claimed, scope, spin_lock, try_claim, waiter};
use skein_rt::object::cons;
use skein_rt::print::pr_str;
use skein_rt::runtime::{Namespace, rt};
use skein_rt::safepoint;
use skein_rt::seq::SeqIter;
use skein_rt::string::str_new;
use skein_rt::vector::vec_from_slice;
use skein_rt::{Kind, Value};
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---- atomic heap fields -------------------------------------------------------

/// Fields read by other threads without the owner's lock (PARK, JOINING,
/// WAKE_*). Sequentially consistent: parking and cancellation rely on a
/// store-then-load handshake on PARK/JOINING and CANCEL (see `cancel`).
pub(crate) fn aset(obj: Value, field: usize, v: Value) {
    obj.atomic_word(field).store(v.0, Ordering::SeqCst);
}

pub(crate) fn aget(obj: Value, field: usize) -> Value {
    Value(obj.atomic_word(field).load(Ordering::SeqCst))
}

fn list_items(v: Value) -> Vec<Value> {
    SeqIter::new(v).map(|it| it.collect()).unwrap_or_default()
}

// ---- root tasks -----------------------------------------------------------------

/// Work submitted from outside the carriers (REPL, tests, `skein FILE`).
pub enum RootTask {
    /// Evaluate a string; the result is the printed last value.
    Eval(String),
    /// Evaluate a string, printing each form's value (the REPL).
    Repl(String),
    /// Load a file.
    Load(String),
    /// An editor-protocol request (one line of Skein data); the result is the
    /// response line, returned raw.
    Request(String),
    /// Like Eval, but a string result is returned raw (not printed readably).
    EvalRaw(String),
    /// Call core function `name` with one string argument, compiling
    /// nothing (an image's start, which may have no compiler: shaken out);
    /// the result is the printed value.
    Call(&'static str, String),
}

pub struct RootDone {
    pub result: Result<String, Error>,
    /// The namespace the fiber ended in (REPL `in-ns`).
    pub ns: String,
}

enum Job {
    Root { task: RootTask, ns: String, ticket: u64 },
    Cancel { ticket: u64, code: i64 },
}

enum TimerKind {
    Wake,
    Close,
}

struct Timers {
    heap: BinaryHeap<Reverse<(Instant, u64)>>,
    entries: HashMap<u64, (TimerKind, Value)>,
    seq: u64,
}

// ---- the scheduler ------------------------------------------------------------------

pub struct Sched {
    /// Configured carriers (SKEIN_THREADS, or one per core).
    pub n: usize,
    /// Carrier slots: the `n` regular carriers, then `n` spares.
    slots: usize,
    /// Per slot: when its carrier entered a native region (ms since start,
    /// plus one), or 0.
    native_since: Vec<AtomicU64>,
    /// Per slot: is a carrier thread running there?
    active: Vec<AtomicBool>,
    /// Values native code is using (tail-call arguments live only in Rust).
    native_keep: Mutex<HashMap<u64, Vec<Value>>>,
    locals: Vec<Mutex<VecDeque<Value>>>,
    injector: Mutex<VecDeque<Value>>,
    idle: Mutex<()>,
    idle_cv: Condvar,
    idle_count: AtomicUsize,
    /// Carriers looking for work a little longer before they sleep
    /// (`spin_for_work`). While there's one, making a fiber runnable wakes
    /// nobody: the spinner finds it.
    spinning: AtomicUsize,
    /// Fibers in the run queues (the carriers' and the global one), counted
    /// before each is pushed: zero means there's nothing to look for, with
    /// no queue's lock taken.
    queued: AtomicUsize,
    timers: Mutex<Timers>,
    timer_cv: Condvar,
    external: Mutex<VecDeque<Job>>,
    external_len: AtomicUsize,
    currents: Vec<AtomicU64>,
    roots: Mutex<HashMap<u64, Value>>,
    /// Cancels that came before their root fiber was registered (one
    /// carrier took the Root job while another took the Cancel): applied
    /// when it is.
    early_cancels: Mutex<HashMap<u64, i64>>,
    notifies: Mutex<HashMap<u64, (mpsc::Sender<RootDone>, bool)>>,
    next_id: AtomicU64,
    paused: AtomicBool,
    in_flight: AtomicUsize,
    pub spawned: AtomicU64,
    pub finished: AtomicU64,
}

static SCHED: OnceLock<Sched> = OnceLock::new();

thread_local! {
    static CARRIER: Cell<usize> = const { Cell::new(usize::MAX) };
}

pub fn sched() -> &'static Sched {
    SCHED.get().expect("scheduler not started")
}

impl Sched {
    fn new(n: usize) -> Sched {
        let slots = 2 * n;
        Sched {
            n,
            slots,
            native_since: (0..slots).map(|_| AtomicU64::new(0)).collect(),
            active: (0..slots).map(|i| AtomicBool::new(i < n)).collect(),
            native_keep: Mutex::new(HashMap::new()),
            locals: (0..slots).map(|_| Mutex::new(VecDeque::new())).collect(),
            injector: Mutex::new(VecDeque::new()),
            idle: Mutex::new(()),
            idle_cv: Condvar::new(),
            idle_count: AtomicUsize::new(0),
            spinning: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            timers: Mutex::new(Timers { heap: BinaryHeap::new(), entries: HashMap::new(), seq: 0 }),
            timer_cv: Condvar::new(),
            external: Mutex::new(VecDeque::new()),
            external_len: AtomicUsize::new(0),
            currents: (0..slots).map(|_| AtomicU64::new(Value::NIL.0)).collect(),
            roots: Mutex::new(HashMap::new()),
            early_cancels: Mutex::new(HashMap::new()),
            notifies: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            paused: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
            spawned: AtomicU64::new(0),
            finished: AtomicU64::new(0),
        }
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn wake_idle(&self) {
        // (a spinner sees the new work, or goes to sleep through the idle
        // path's last look under the lock, after it stops counting as one)
        if self.spinning.load(Ordering::SeqCst) > 0 {
            return;
        }
        if self.idle_count.load(Ordering::SeqCst) > 0 {
            let _g = self.idle.lock().unwrap();
            self.idle_cv.notify_one();
        }
    }

    /// Visit every GC root held outside the heap. Called with the world stopped.
    pub fn for_each_root(&self, f: &mut dyn FnMut(Value)) {
        for q in &self.locals {
            q.lock().unwrap().iter().for_each(|&v| f(v));
        }
        self.injector.lock().unwrap().iter().for_each(|&v| f(v));
        self.timers.lock().unwrap().entries.values().for_each(|(_, v)| f(*v));
        self.roots.lock().unwrap().values().for_each(|&v| f(v));
        self.native_keep.lock().unwrap().values().flatten().for_each(|&v| f(v));
        crate::jit::for_each_linked(f);
        crate::jit::for_each_queued(f);
        // (the functions unwinding knows by their addresses, kept in a
        // static: as roots they stay where they are, M11)
        if let Some(k) = crate::vm::FRAME_KINDS.get() {
            [k.catch, k.unwind, k.bind, k.finish, k.runtime_error].into_iter().for_each(&mut *f);
        }
        crate::reactor::for_each_root(f);
        for c in &self.currents {
            let v = Value(c.load(Ordering::Acquire));
            if !v.is_nil() {
                f(v);
            }
        }
    }
}

/// Make a fiber runnable (its state is already QUEUED).
pub fn schedule(f: Value) {
    let s = sched();
    let me = CARRIER.with(|c| c.get());
    s.queued.fetch_add(1, Ordering::SeqCst);
    if me < s.slots {
        s.locals[me].lock().unwrap().push_back(f);
    } else {
        s.injector.lock().unwrap().push_back(f);
    }
    s.wake_idle();
}

/// Requeue a fiber whose time slice ran out. It goes to the *global* queue,
/// behind everything already waiting, so a busy fiber can't starve fibers
/// woken from outside the carriers (timers, other threads).
fn requeue(f: Value) {
    let s = sched();
    s.queued.fetch_add(1, Ordering::SeqCst);
    s.injector.lock().unwrap().push_back(f);
    s.wake_idle();
}

thread_local! {
    static PICKS: Cell<u32> = const { Cell::new(0) };
}

fn next_fiber(me: usize) -> Option<Value> {
    let s = sched();
    // (the count is raised before a push, so zero is nothing queued; and
    // whoever pushes next wakes a sleeper or leaves it to a spinner)
    if s.queued.load(Ordering::SeqCst) == 0 {
        return None;
    }
    let f = take_fiber(s, me);
    if f.is_some() {
        s.queued.fetch_sub(1, Ordering::SeqCst);
    }
    f
}

fn take_fiber(s: &Sched, me: usize) -> Option<Value> {
    // Every 61st pick, look at the global queue first (as Go does), so local
    // work can never starve it.
    let tick = PICKS.with(|p| {
        let n = p.get().wrapping_add(1);
        p.set(n);
        n
    });
    if tick.is_multiple_of(61)
        && let Some(f) = s.injector.lock().unwrap().pop_front()
    {
        return Some(f);
    }
    if let Some(f) = s.locals[me].lock().unwrap().pop_front() {
        return Some(f);
    }
    if let Some(f) = s.injector.lock().unwrap().pop_front() {
        return Some(f);
    }
    for k in 1..s.slots {
        let victim = (me + k) % s.slots;
        if let Some(f) = s.locals[victim].lock().unwrap().pop_back() {
            return Some(f);
        }
    }
    None
}

/// Is anything else waiting to run? (Decides whether a time slice yields.)
pub fn has_other_work(me: usize) -> bool {
    let s = sched();
    (me < s.slots && !s.locals[me].lock().unwrap().is_empty()) || !s.injector.lock().unwrap().is_empty()
}

// ---- native regions, the monitor thread and spare carriers ----------------------------

fn now_ms() -> u64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// A native region of carrier `me` starts: it stops counting as running for
/// the GC (the caller has saved its stack and must not touch the heap).
pub(crate) fn enter_native(me: usize) {
    let s = sched();
    if me < s.slots {
        s.native_since[me].store(now_ms(), Ordering::Release);
        // Wake the monitor if it parked while no carrier was in native code.
        std::sync::atomic::fence(Ordering::SeqCst);
        if MONITOR_PARKED.load(Ordering::Relaxed)
            && let Some(t) = MONITOR.get()
        {
            t.unpark();
        }
    }
    safepoint::enter_safe();
}

/// The native region ends (waits out a GC in progress).
pub(crate) fn leave_native(me: usize) {
    safepoint::leave_safe();
    let s = sched();
    if me < s.slots {
        s.native_since[me].store(0, Ordering::Release);
    }
}

/// Keep `vs` alive until `unroot_many` (values only native code holds).
pub(crate) fn root_many(vs: &[Value]) -> u64 {
    let s = sched();
    let k = s.next_id();
    s.native_keep.lock().unwrap().insert(k, vs.to_vec());
    k
}

pub(crate) fn unroot_many(k: u64) {
    sched().native_keep.lock().unwrap().remove(&k);
}

/// How long a carrier may sit in a native region before it counts as stuck.
const STUCK_MS: u64 = 20;

fn stuck_carriers(s: &Sched) -> usize {
    let now = now_ms();
    (0..s.slots)
        .filter(|&i| {
            let t = s.native_since[i].load(Ordering::Acquire);
            s.active[i].load(Ordering::Acquire) && t != 0 && now - t > STUCK_MS
        })
        .count()
}

fn active_carriers(s: &Sched) -> usize {
    s.active.iter().filter(|a| a.load(Ordering::Acquire)).count()
}

fn work_waiting(s: &Sched) -> bool {
    !s.injector.lock().unwrap().is_empty() || s.locals.iter().any(|q| !q.lock().unwrap().is_empty())
}

/// The monitor thread, and whether it is parked (no carrier is in native
/// code, so none can get stuck; enter_native wakes it).
static MONITOR: OnceLock<std::thread::Thread> = OnceLock::new();
static MONITOR_PARKED: AtomicBool = AtomicBool::new(false);

fn in_native(s: &Sched) -> bool {
    s.native_since.iter().any(|t| t.load(Ordering::Acquire) != 0)
}

/// Every 10 ms: if carriers are stuck in native code while fibers wait, start
/// a spare so that `n` carriers are running fibers again. While no carrier is
/// in native code it parks, so an idle process doesn't wake 100 times a
/// second.
fn monitor_main() {
    let s = sched();
    let _ = MONITOR.set(std::thread::current());
    loop {
        if !in_native(s) {
            MONITOR_PARKED.store(true, Ordering::Relaxed);
            std::sync::atomic::fence(Ordering::SeqCst);
            if !in_native(s) {
                std::thread::park();
            }
            MONITOR_PARKED.store(false, Ordering::Relaxed);
            continue;
        }
        std::thread::sleep(Duration::from_millis(10));
        let stuck = stuck_carriers(s);
        if stuck == 0 || active_carriers(s) - stuck >= s.n || !work_waiting(s) {
            continue;
        }
        if let Some(slot) = (s.n..s.slots).find(|&i| !s.active[i].load(Ordering::Acquire)) {
            s.active[slot].store(true, Ordering::Release);
            start_carrier(slot, None, None);
        }
    }
}

/// Should spare carrier `me` retire? (It's idle, and the regular carriers
/// can cope again.)
fn should_retire(s: &Sched, me: usize) -> bool {
    me >= s.n && active_carriers(s) - stuck_carriers(s) > s.n
}

/// Number of carrier threads running now (regular and spare).
pub fn carrier_threads() -> usize {
    active_carriers(sched())
}

// ---- pausing (image saves) ---------------------------------------------------------

/// Is an image save pausing the scheduler? Running fibers yield when it is.
pub fn pausing() -> bool {
    SCHED.get().is_some_and(|s| s.paused.load(Ordering::Relaxed))
}

/// Start a unit of carrier work (picking a fiber, handling external jobs)
/// unless the scheduler is paused. Pair with `end_work`.
fn begin_work(s: &Sched) -> bool {
    s.in_flight.fetch_add(1, Ordering::SeqCst);
    if s.paused.load(Ordering::SeqCst) {
        s.in_flight.fetch_sub(1, Ordering::SeqCst);
        return false;
    }
    true
}

fn end_work(s: &Sched) {
    s.in_flight.fetch_sub(1, Ordering::SeqCst);
}

/// Stop the carriers: no fiber runs once this returns `Ok`. Fibers that were
/// running yield at their next poll (back to the run queue, resumable). Gives
/// up after `limit` (a fiber stuck in a long native call can't be stopped).
pub(crate) fn pause_all(limit: Duration) -> Result<(), String> {
    let s = sched();
    if s.paused.swap(true, Ordering::SeqCst) {
        return Err("another image save is in progress".into());
    }
    safepoint::bump_attention();
    let deadline = Instant::now() + limit;
    while s.in_flight.load(Ordering::SeqCst) != 0 {
        if Instant::now() > deadline {
            resume_all();
            return Err("couldn't stop the running fibers (one may be inside a long native call)".into());
        }
        std::thread::sleep(Duration::from_micros(100));
    }
    Ok(())
}

pub(crate) fn resume_all() {
    let s = sched();
    s.paused.store(false, Ordering::SeqCst);
    let _g = s.idle.lock().unwrap();
    s.idle_cv.notify_all();
}

/// While paused: the runnable fibers, in queue order.
pub(crate) fn queued_fibers() -> Vec<Value> {
    let s = sched();
    let mut v: Vec<Value> = Vec::new();
    for q in &s.locals {
        v.extend(q.lock().unwrap().iter().copied());
    }
    v.extend(s.injector.lock().unwrap().iter().copied());
    v
}

/// A pending timer as an image saves it.
pub(crate) struct SavedTimer {
    pub ms_left: u64,
    /// true: wake a sleeping fiber's waiter; false: close a `timeout` channel.
    pub wake: bool,
    pub target: Value,
}

/// Threads outside the carriers (the reactor, the blocking pool) wake fibers
/// inside `delivering`; an image save holds them all off at once.
static DELIVERIES: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// Run `f` (which wakes fibers) as a running participant, unless an image
/// save is in progress, in which case wait for it.
pub(crate) fn delivering<R>(f: impl FnOnce() -> R) -> R {
    let _g = DELIVERIES.read().unwrap();
    safepoint::while_running(f)
}

/// Run `f` with the reactor and the blocking pool held off (but not the
/// timer thread: `f` may take the timer lock itself, e.g. to list roots).
pub(crate) fn with_deliveries_held<R>(f: impl FnOnce() -> R) -> R {
    let _g = DELIVERIES.write().unwrap();
    f()
}

/// Run `f` with the timer thread, the reactor and the blocking pool held
/// off (they wake fibers, which would change the heap under an image save),
/// given the pending timers in due order.
pub(crate) fn with_timers_held<R>(f: impl FnOnce(Vec<SavedTimer>) -> R) -> R {
    let s = sched();
    let _deliveries = DELIVERIES.write().unwrap();
    let t = s.timers.lock().unwrap();
    let now = Instant::now();
    let mut due: Vec<(Instant, u64)> = t.heap.iter().map(|r| r.0).collect();
    due.sort();
    let saved = due
        .into_iter()
        .filter_map(|(at, seq)| {
            t.entries.get(&seq).map(|(k, v)| SavedTimer {
                ms_left: at.saturating_duration_since(now).as_millis() as u64,
                wake: matches!(k, TimerKind::Wake),
                target: *v,
            })
        })
        .collect();
    let r = f(saved);
    drop(t);
    r
}

/// Is this thread a carrier? (Carriers are already running participants:
/// they must never go through `delivering`, which could wait for a
/// collection that is waiting for them.)
pub(crate) fn on_carrier() -> bool {
    CARRIER.with(|c| c.get()) != usize::MAX
}

/// Is waiter `w` waiting for a timer (a sleeping fiber)?
pub(crate) fn timer_holds(w: Value) -> bool {
    sched().timers.lock().unwrap().entries.values().any(|(k, v)| matches!(k, TimerKind::Wake) && *v == w)
}

/// Re-arm a timer from an image.
pub(crate) fn restore_timer(t: &SavedTimer) {
    let kind = if t.wake { TimerKind::Wake } else { TimerKind::Close };
    add_timer(Instant::now() + Duration::from_millis(t.ms_left), kind, t.target);
}

pub(crate) fn fiber_id_counter() -> u64 {
    sched().next_id.load(Ordering::Relaxed)
}

/// Keep `v` alive while only Rust code holds it (returns a key for `unroot`).
pub(crate) fn root(v: Value) -> u64 {
    let s = sched();
    let k = s.next_id();
    s.roots.lock().unwrap().insert(k, v);
    k
}

pub(crate) fn unroot(k: u64) {
    sched().roots.lock().unwrap().remove(&k);
}

/// The error a fiber saved dead in an image ends with after the restore.
pub const NOT_SAVED: &str = "Fiber was not saved in the image";

/// End a fiber that an image saved dead (waking its joiners and scope).
pub(crate) fn finish_unsaved(vm: &mut Vm, f: Value) {
    finish(vm, f, Err(Error::new(NOT_SAVED)));
}

// ---- fibers -------------------------------------------------------------------------

/// Create a fiber running `thunk` (a callable, or an internal `[f arg…]`
/// array) and make it runnable.
pub fn spawn(vm: &mut Vm, thunk: Value, scope_v: Value) -> Value {
    spawn_in(vm, thunk, scope_v, false)
}

/// Like `spawn`, for a scope's body (`with-scope`): the scope then judges a
/// failure nobody waited for by what its body does next (see `scope_child_done`).
pub fn spawn_body(vm: &mut Vm, thunk: Value, scope_v: Value) -> Value {
    spawn_in(vm, thunk, scope_v, true)
}

fn spawn_in(vm: &mut Vm, thunk: Value, scope_v: Value, body: bool) -> Value {
    let s = sched();
    let f = fiber_new(&mut vm.m, s.next_id(), thunk, scope_v, vm.ns as *const Namespace as usize);
    // Conveyance: the child starts in its creator's dynamic environment
    // (bindings, condition handlers, restarts, debugger mode).
    f.set_field(fiber::DYN, vm.dyn_env);
    if !scope_v.is_nil() {
        let _g = spin_lock(scope_v, scope::LOCK);
        let kids = cons(&mut vm.m, f, scope_v.field(scope::CHILDREN));
        scope_v.set_field(scope::CHILDREN, kids);
        scope_v.set_word(scope::PENDING, scope_v.word(scope::PENDING) + 1);
        if body && scope_v.field(scope::BODY).is_nil() {
            scope_v.set_field(scope::BODY, f);
        }
        // A scope that has been cancelled (it failed, or its owner was) has
        // already cancelled its children: one spawned now, by a fiber still
        // unwinding, starts cancelled and never runs its body.
        if scope_v.word(scope::FLAGS) & scope::CANCELLED != 0 {
            f.atomic_word(fiber::CANCEL).store(fiber::CANCELLED as u64, Ordering::SeqCst);
        }
    }
    s.spawned.fetch_add(1, Ordering::Relaxed);
    schedule(f);
    f
}

/// Transition a woken fiber towards running.
fn wake_fiber(f: Value) {
    let st = f.fiber_state();
    loop {
        match st.load(Ordering::Acquire) {
            fiber::RUNNING => {
                if st.compare_exchange(fiber::RUNNING, fiber::NOTIFIED, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    return;
                }
            }
            fiber::PARKED => {
                if st.compare_exchange(fiber::PARKED, fiber::QUEUED, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    schedule(f);
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Wake the fiber of a waiter whose claim the caller has won.
pub fn deliver(w: Value, value: Value, aux: Value) {
    let f = w.field(waiter::FIBER);
    aset(f, fiber::WAKE_VALUE, value);
    aset(f, fiber::WAKE_AUX, aux);
    wake_fiber(f);
}

/// Park the current fiber on waiter `w` (the primitive then returns).
pub fn park_on(vm: &mut Vm, w: Value, pair: bool) {
    aset(vm.current, fiber::PARK, w);
    vm.suspend = Some(crate::vm::Suspend::Park { pair });
}

/// The error a fiber raises when woken with WAKE_ERR set.
pub fn wake_error(err: Value) -> Error {
    if err.is_fixnum() {
        cancel_error(err.as_fixnum())
    } else if err.is(Kind::Fiber) {
        error_from_fiber(err)
    } else if err.is(Kind::Vector) && err.vec_count() == 3 {
        // (a blocking job's failure with its class: [message class data])
        let mut e = Error::new(pr_str(err.vec_nth(0), false));
        e.class = Some(crate::vm::ErrClass::Value(err));
        e
    } else {
        Error::new(pr_str(err, false))
    }
}

/// Rebuild the error a fiber died with: its condition, signalled again
/// (class and data intact), when it ended with one.
pub fn error_from_fiber(f: Value) -> Error {
    let mut e = Error::new(pr_str(f.field(fiber::ERROR), false));
    let c = f.field(fiber::CONDITION);
    if !c.is_nil() {
        e.class = Some(crate::vm::ErrClass::Value(c));
    }
    let trace = f.field(fiber::TRACE);
    if trace.is(Kind::Vector) {
        trace.vec_for_each(|t| {
            e.trace.push(pr_str(t, false));
            true
        });
    }
    e.trace.push(format!("(in fiber {})", f.fiber_id()));
    e
}

/// The outcome of a finished fiber.
pub fn fiber_outcome(f: Value) -> Result<Value, Error> {
    if f.field(fiber::ERROR).is_nil() { Ok(f.field(fiber::RESULT)) } else { Err(error_from_fiber(f)) }
}

fn finish(vm: &mut Vm, f: Value, r: Result<Value, Error>) {
    let s = sched();
    let (val, err) = match r {
        Ok(v) => (v, None),
        Err(e) => (Value::NIL, Some(e)),
    };
    let (emsg, etrace, econd) = match &err {
        Some(e) => {
            let msg = str_new(&mut vm.m, &e.msg);
            let lines: Vec<Value> = e.trace.iter().map(|t| str_new(&mut vm.m, t)).collect();
            let cond = match &e.class {
                Some(crate::vm::ErrClass::Value(c)) if !c.is(Kind::Vector) => *c,
                _ => Value::NIL,
            };
            (msg, vec_from_slice(&mut vm.m, &lines), cond)
        }
        None => (Value::NIL, Value::NIL, Value::NIL),
    };
    let sc = f.field(fiber::SCOPE);
    let cancel_end = matches!(&err, Some(e) if is_cancel_end(f, e));
    // (a scope's child failing with nobody waiting: in its scope's UNSEEN
    // before anyone can see it DONE, so an await or settle that sees it
    // DONE finds it there to take out; see scope_child_done)
    let mut pre_unseen = false;
    let joiners = {
        let _g = spin_lock(f, fiber::LOCK);
        f.set_field(fiber::RESULT, val);
        f.set_field(fiber::ERROR, emsg);
        f.set_field(fiber::TRACE, etrace);
        f.set_field(fiber::CONDITION, econd);
        f.set_field(fiber::SEG, Value::NIL);
        aset(f, fiber::PARK, Value::NIL);
        let j = f.field(fiber::JOINERS);
        if err.is_some() && !cancel_end && j.is_nil() && !sc.is_nil() && sc.field(scope::BODY) != f {
            let _sg = spin_lock(sc, scope::LOCK);
            let unseen = cons(&mut vm.m, f, sc.field(scope::UNSEEN));
            sc.set_field(scope::UNSEEN, unseen);
            pre_unseen = true;
        }
        f.fiber_state().store(fiber::DONE, Ordering::Release);
        f.set_field(fiber::JOINERS, Value::NIL);
        j
    };
    // (an await or settle that receives the outcome has seen the error)
    let mut seen = false;
    for w in list_items(joiners) {
        if try_claim(w.field(waiter::CLAIM)) {
            if err.is_some() && w.word(waiter::FLAGS) & waiter::NO_RAISE == 0 {
                aset(w.field(waiter::FIBER), fiber::WAKE_ERR, f);
            }
            deliver(w, val, Value::NIL);
            seen = true;
        }
    }
    if !sc.is_nil() {
        let end = match &err {
            None => ChildEnd::Ok,
            // stopped by cancel! (or by its scope's own cancelling): quiet
            Some(_) if cancel_end => ChildEnd::Cancelled,
            Some(_) if seen => ChildEnd::Seen,
            Some(_) => ChildEnd::Unseen,
        };
        scope_child_done(vm, sc, f, end, pre_unseen);
    }
    let ticket = f.word(fiber::NOTIFY);
    if ticket != 0 {
        s.roots.lock().unwrap().remove(&ticket);
        if let Some((tx, raw)) = s.notifies.lock().unwrap().remove(&ticket) {
            let result = match err {
                Some(e) => Err(e),
                None if raw && val.is(Kind::String) => Ok(val.as_str().to_owned()),
                None => Ok(pr_str(val, true)),
            };
            let _ = tx.send(RootDone { result, ns: vm.ns.name.sym_name().to_owned() });
        }
    } else if let Some(e) = &err
        && sc.is_nil()
        && e.msg != "Cancelled"
        && e.msg != NOT_SAVED
    {
        // A detached fiber died: say so, as Erlang does (it can still be awaited).
        eprintln!("Fiber {} failed: {e}", f.fiber_id());
    }
    s.finished.fetch_add(1, Ordering::Relaxed);
}

/// Cancel a fiber: it raises `Cancelled` (or `Interrupted`) at its next
/// safepoint, or immediately if parked. Cancelling a fiber that is joining a
/// scope cancels the scope's children too.
///
/// Handshake with a fiber that is about to park: the canceller stores CANCEL
/// then loads PARK; the parker stores PARK then loads CANCEL (in `run_one`,
/// before committing the park). With sequentially consistent accesses at
/// least one side sees the other, and the waiter's claim makes sure exactly
/// one of them delivers the wake-up.
pub fn cancel(f: Value, code: i64) {
    if f.fiber_state().load(Ordering::Acquire) == fiber::DONE {
        return;
    }
    let _ = f.atomic_word(fiber::CANCEL).compare_exchange(0, code as u64, Ordering::SeqCst, Ordering::SeqCst);
    safepoint::bump_attention();
    let joining = aget(f, fiber::JOINING);
    if !joining.is_nil() {
        cancel_children(joining);
    }
    let w = aget(f, fiber::PARK);
    if !w.is_nil() && w.word(waiter::FLAGS) & waiter::NO_CANCEL_WAKE == 0 && try_claim(w.field(waiter::CLAIM)) {
        aset(f, fiber::WAKE_ERR, Value::fix(code));
        deliver(w, Value::NIL, Value::NIL);
    }
}

// ---- scopes ----------------------------------------------------------------------------
//
// A scope fails only for an error nobody handled:
//
// - A child stopped by `cancel!` ends quietly (`ChildEnd::Cancelled`).
// - A child whose error an `await` or `settle` received when it ended
//   (`ChildEnd::Seen`) doesn't fail the scope: the waiter has it.
// - A child that fails with nobody waiting (`ChildEnd::Unseen`) fails the
//   scope at once (cancelling the rest) if the scope's body is waiting or
//   has finished. While the body is still running it may yet await or
//   settle that child, so the failure waits in UNSEEN: an await or settle
//   of the finished child takes it out (`observe`); the body's next park, or
//   its end, fails the scope with the oldest one left. That keeps
//   `(settle (spawn s …))` the same however the two fibers are scheduled.

#[derive(Clone, Copy, PartialEq)]
enum ChildEnd {
    Ok,
    Cancelled,
    Seen,
    Unseen,
}

/// Did fiber `f` end because it was cancelled (and not with some other
/// error raised while it unwound)? Cancellation is no condition, so the
/// error has no class.
fn is_cancel_end(f: Value, e: &Error) -> bool {
    let code = match f.fiber_cancel() {
        0 => f.word(fiber::CANCELLED_WITH),
        c => c,
    };
    code != 0 && e.class.is_none() && e.msg == cancel_error(code as i64).msg
}

fn cancel_children(sc: Value) {
    let kids = {
        let _g = spin_lock(sc, scope::LOCK);
        sc.set_word(scope::FLAGS, sc.word(scope::FLAGS) | scope::CANCELLED);
        sc.field(scope::CHILDREN)
    };
    for k in list_items(kids) {
        cancel(k, fiber::CANCELLED);
    }
}

/// The oldest failure in UNSEEN, made the scope's failure (under its lock).
/// True if there was one: the caller then cancels the children.
fn fail_with_unseen(sc: Value) -> bool {
    let unseen = sc.field(scope::UNSEEN);
    if unseen.is_nil() || !sc.field(scope::FAILED).is_nil() {
        return false;
    }
    let oldest = list_items(unseen).last().copied().unwrap_or(Value::NIL);
    sc.set_field(scope::UNSEEN, Value::NIL);
    sc.set_field(scope::FAILED, oldest);
    true
}

/// An await or settle has received finished fiber `f`'s error: if it was
/// waiting in its scope's UNSEEN, it no longer fails the scope.
pub(crate) fn observe(vm: &mut Vm, f: Value) {
    let sc = f.field(fiber::SCOPE);
    if sc.is_nil() || f.field(fiber::ERROR).is_nil() {
        return;
    }
    let _g = spin_lock(sc, scope::LOCK);
    let items = list_items(sc.field(scope::UNSEEN));
    if items.contains(&f) {
        let mut rest = Value::NIL;
        for &x in items.iter().rev() {
            if x != f {
                rest = cons(&mut vm.m, x, rest);
            }
        }
        sc.set_field(scope::UNSEEN, rest);
    }
}

/// The scope's body is about to park: a failure nobody saw fails the scope now.
fn body_waits(sc: Value) {
    let fail = {
        let _g = spin_lock(sc, scope::LOCK);
        sc.set_word(scope::FLAGS, sc.word(scope::FLAGS) | scope::BODY_WAITING);
        fail_with_unseen(sc)
    };
    if fail {
        cancel_children(sc);
    }
}

/// The scope's body runs again.
fn body_runs(sc: Value) {
    let _g = spin_lock(sc, scope::LOCK);
    sc.set_word(scope::FLAGS, sc.word(scope::FLAGS) & !scope::BODY_WAITING);
}

/// List `l` without `x`, in order.
fn list_without(vm: &mut Vm, l: Value, x: Value) -> Value {
    let mut rest = Value::NIL;
    for &y in list_items(l).iter().rev() {
        if y != x {
            rest = cons(&mut vm.m, y, rest);
        }
    }
    rest
}

/// Child `child` of scope `sc` has ended. `pre_unseen`: finish put it in
/// UNSEEN already (it failed with nobody waiting), so if it isn't there
/// now an await or settle has taken it out: seen.
fn scope_child_done(vm: &mut Vm, sc: Value, child: Value, end: ChildEnd, pre_unseen: bool) {
    let (owner, cancel_siblings) = {
        let _g = spin_lock(sc, scope::LOCK);
        sc.set_word(scope::PENDING, sc.word(scope::PENDING) - 1);
        let body = sc.field(scope::BODY);
        let is_body = body == child;
        let mut cancel_siblings = false;
        let still_unseen = !pre_unseen || list_items(sc.field(scope::UNSEEN)).contains(&child);
        if end == ChildEnd::Unseen && still_unseen && sc.field(scope::FAILED).is_nil() {
            let body_running = !body.is_nil()
                && !is_body
                && sc.word(scope::FLAGS) & scope::BODY_WAITING == 0
                && body.fiber_state().load(Ordering::Acquire) != fiber::DONE;
            if body_running {
                if !pre_unseen {
                    let unseen = cons(&mut vm.m, child, sc.field(scope::UNSEEN));
                    sc.set_field(scope::UNSEEN, unseen);
                }
            } else {
                if pre_unseen {
                    let rest = list_without(vm, sc.field(scope::UNSEEN), child);
                    sc.set_field(scope::UNSEEN, rest);
                }
                sc.set_field(scope::FAILED, child);
                cancel_siblings = true;
            }
        }
        if is_body && fail_with_unseen(sc) {
            cancel_siblings = true;
        }
        let owner = if sc.word(scope::PENDING) == 0 {
            let o = sc.field(scope::OWNER);
            sc.set_field(scope::OWNER, Value::NIL);
            o
        } else {
            Value::NIL
        };
        (owner, cancel_siblings)
    };
    if cancel_siblings {
        cancel_children(sc);
    }
    if !owner.is_nil() && try_claim(owner.field(waiter::CLAIM)) {
        deliver(owner, Value::NIL, Value::NIL);
    }
}

/// `(%scope-join s)`: wait until every child of the scope has finished.
pub fn scope_join(vm: &mut Vm, sc: Value) -> Result<Value, Error> {
    if !vm.can_park() {
        return Err(Vm::park_error());
    }
    let me = vm.current;
    let claim = skein_rt::conc::claim_new(&mut vm.m);
    let w = skein_rt::conc::waiter_new(&mut vm.m, me, Value::NIL, claim, Value::NIL, waiter::NO_CANCEL_WAKE);
    {
        let _g = spin_lock(sc, scope::LOCK);
        if sc.word(scope::PENDING) == 0 {
            return Ok(Value::NIL);
        }
        sc.set_field(scope::OWNER, w);
    }
    aset(me, fiber::JOINING, sc);
    // A cancel that arrived before JOINING was visible must still reach the children.
    if me.fiber_cancel() != 0 {
        cancel_children(sc);
    }
    park_on(vm, w, false);
    Ok(Value::NIL)
}

// ---- timers --------------------------------------------------------------------------------

fn add_timer(at: Instant, kind: TimerKind, target: Value) {
    let s = sched();
    let mut t = s.timers.lock().unwrap();
    t.seq += 1;
    let seq = t.seq;
    t.heap.push(Reverse((at, seq)));
    t.entries.insert(seq, (kind, target));
    s.timer_cv.notify_one();
}

/// `(sleep ms)`.
pub fn sleep(vm: &mut Vm, ms: u64) -> Result<Value, Error> {
    if !vm.can_park() {
        return Err(Vm::park_error());
    }
    let claim = skein_rt::conc::claim_new(&mut vm.m);
    let w = skein_rt::conc::waiter_new(&mut vm.m, vm.current, Value::NIL, claim, Value::NIL, 0);
    park_on(vm, w, false);
    add_timer(Instant::now() + Duration::from_millis(ms), TimerKind::Wake, w);
    Ok(Value::NIL)
}

/// `(timeout ms)`: a channel that closes after `ms`.
pub fn timeout(vm: &mut Vm, ms: u64) -> Value {
    let c = skein_rt::conc::chan_new(&mut vm.m, 0);
    add_timer(Instant::now() + Duration::from_millis(ms), TimerKind::Close, c);
    c
}

fn timer_main() {
    let s = sched();
    let mut guard = s.timers.lock().unwrap();
    loop {
        let now = Instant::now();
        match guard.heap.peek() {
            Some(Reverse((at, _))) if *at <= now => {
                drop(guard);
                // Fire everything due while counted as running, so a GC
                // can't run while timer targets are off the root set.
                safepoint::while_running(|| {
                    let mut t = s.timers.lock().unwrap();
                    while let Some(Reverse((at, seq))) = t.heap.peek().copied() {
                        if at > Instant::now() {
                            break;
                        }
                        t.heap.pop();
                        if let Some((kind, v)) = t.entries.remove(&seq) {
                            match kind {
                                TimerKind::Wake => {
                                    if try_claim(v.field(waiter::CLAIM)) {
                                        deliver(v, Value::NIL, Value::NIL);
                                    }
                                }
                                TimerKind::Close => crate::conc::close(v),
                            }
                        }
                    }
                });
                guard = s.timers.lock().unwrap();
            }
            Some(Reverse((at, _))) => {
                let wait = *at - now;
                guard = s.timer_cv.wait_timeout(guard, wait).unwrap().0;
            }
            None => guard = s.timer_cv.wait(guard).unwrap(),
        }
    }
}

// ---- external jobs ------------------------------------------------------------------------

fn submit(job: Job) {
    let s = sched();
    s.external.lock().unwrap().push_back(job);
    s.external_len.fetch_add(1, Ordering::Release);
    safepoint::bump_attention();
    let _g = s.idle.lock().unwrap();
    s.idle_cv.notify_all();
}

/// Handle jobs from outside the carriers. Called by idle carriers and from
/// the slow safepoint path, so busy carriers still see cancellations.
pub fn process_external(vm: &mut Vm) {
    let s = sched();
    // Cleanups of collected objects (skein-rt weak.rs) each run in a fiber.
    for f in skein_rt::weak::take_ready() {
        spawn(vm, f, Value::NIL);
    }
    if s.external_len.load(Ordering::Acquire) == 0 {
        return;
    }
    loop {
        let job = s.external.lock().unwrap().pop_front();
        let Some(job) = job else { break };
        s.external_len.fetch_sub(1, Ordering::AcqRel);
        match job {
            Job::Root { task, ns, ticket } => {
                let (fname, arg) = match task {
                    RootTask::Eval(src) => ("%eval-root", src),
                    RootTask::Repl(src) => ("%repl-eval", src),
                    RootTask::Load(path) => ("load-file", path),
                    RootTask::Request(line) => ("%serve-request", line),
                    RootTask::EvalRaw(src) => ("%eval-root", src),
                    RootTask::Call(name, arg) => (name, arg),
                };
                let sym = rt().symbol(&mut vm.m, None, fname);
                // (a shaken image may lack one: that task ends with an error)
                let Some(var) = vm.core.lookup(sym) else {
                    if let Some((tx, _)) = s.notifies.lock().unwrap().remove(&ticket) {
                        let msg = format!("this image has no skein.core/{fname} (left out when it was shaken?)");
                        let _ = tx.send(RootDone { result: Err(Error::new(msg)), ns: ns.clone() });
                    }
                    continue;
                };
                let arg = str_new(&mut vm.m, &arg);
                let spec = skein_rt::array::array_from(&mut vm.m, &[var, arg]);
                let ns_sym = rt().symbol(&mut vm.m, None, &ns);
                let ns = rt().find_or_create_ns(ns_sym);
                let f = fiber_new(&mut vm.m, ticket, spec, Value::NIL, ns as *const Namespace as usize);
                f.set_word(fiber::NOTIFY, ticket);
                s.roots.lock().unwrap().insert(ticket, f);
                s.spawned.fetch_add(1, Ordering::Relaxed);
                schedule(f);
                if let Some(code) = s.early_cancels.lock().unwrap().remove(&ticket) {
                    cancel(f, code);
                }
            }
            Job::Cancel { ticket, code } => {
                // (under the roots lock, so a Root job registering this
                // ticket now sees the early cancel, or is seen here)
                let roots = s.roots.lock().unwrap();
                match roots.get(&ticket).copied() {
                    Some(f) => {
                        drop(roots);
                        cancel(f, code);
                    }
                    None => {
                        let mut early = s.early_cancels.lock().unwrap();
                        // (a cancel for a ticket already finished is never
                        // claimed: keep that from growing without end)
                        if early.len() > 1024 {
                            early.clear();
                        }
                        early.insert(ticket, code);
                    }
                }
            }
        }
    }
}

// ---- carriers ---------------------------------------------------------------------------------

fn run_one(vm: &mut Vm, f: Value) {
    let s = sched();
    s.currents[vm.carrier].store(f.0, Ordering::Release);
    f.fiber_state().store(fiber::RUNNING, Ordering::Release);
    vm.current = f;
    vm.slice_start = Instant::now();
    // SAFETY: NS holds a leaked &'static Namespace.
    vm.ns = unsafe { &*(f.word(fiber::NS) as *const Namespace) };
    vm.dyn_env = f.field(fiber::DYN);
    // (the scope whose body this fiber runs, if any)
    let body_of = {
        let sc = f.field(fiber::SCOPE);
        if !sc.is_nil() && sc.field(scope::BODY) == f { sc } else { Value::NIL }
    };
    loop {
        aset(f, fiber::PARK, Value::NIL);
        aset(f, fiber::JOINING, Value::NIL);
        if !body_of.is_nil() {
            body_runs(body_of);
        }
        // run_fiber notices a pending cancel itself (and unwinds).
        let r = vm.run_fiber(f);
        f.set_word(fiber::NS, vm.ns as *const Namespace as usize as u64);
        f.set_field(fiber::DYN, vm.dyn_env);
        match r {
            Ok(Exit::Done(v)) => {
                finish(vm, f, Ok(v));
                break;
            }
            Err(e) => {
                finish(vm, f, Err(e));
                break;
            }
            Ok(Exit::Yield) => {
                f.fiber_state().store(fiber::QUEUED, Ordering::Release);
                requeue(f);
                break;
            }
            Ok(Exit::Park) => {
                // (a scope's body parking lets a failure nobody saw fail the
                // scope, which may cancel this fiber: seen just below)
                if !body_of.is_nil() {
                    body_waits(body_of);
                }
                // A cancel that arrived before our waiter was visible in PARK
                // couldn't wake us; we must notice it ourselves, or we would
                // park forever with the cancel flag set.
                if f.fiber_cancel() != 0 {
                    let w = aget(f, fiber::PARK);
                    if !w.is_nil() && w.word(waiter::FLAGS) & waiter::NO_CANCEL_WAKE == 0 && try_claim(w.field(waiter::CLAIM)) {
                        continue; // run_fiber sees the cancel and unwinds
                    }
                }
                match f.fiber_state().compare_exchange(fiber::RUNNING, fiber::PARKED, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => break,
                    // Woken before we finished parking: carry on right away.
                    Err(_) => f.fiber_state().store(fiber::RUNNING, Ordering::Release),
                }
            }
        }
    }
    vm.current = Value::NIL;
    s.currents[vm.carrier].store(Value::NIL.0, Ordering::Release);
}

/// How long a carrier with nothing to run keeps looking before it sleeps.
const SPIN: Duration = Duration::from_micros(50);

/// Out of work: look for more for a moment before sleeping, as one of a
/// few spinners. A fiber that wakes another (a `put!` to a parked taker)
/// then wakes no thread, which cost it a system call each time while any
/// carrier slept: a fan-out through channels ran 20 times slower on ten
/// carriers than on one. The fiber found is returned inside a unit of
/// work (`begin_work`).
fn spin_for_work(s: &Sched, me: usize) -> Option<Value> {
    if s.n < 2 || me >= s.n / 2 {
        return None;
    }
    if s.spinning.fetch_add(1, Ordering::SeqCst) >= s.n / 2 {
        s.spinning.fetch_sub(1, Ordering::SeqCst);
        return None;
    }
    let deadline = Instant::now() + SPIN;
    let mut found = None;
    loop {
        // (a collection, an image save or an outside job: the idle path's)
        if safepoint::stop_requested() || s.external_len.load(Ordering::Acquire) > 0 || !begin_work(s) {
            break;
        }
        if let Some(f) = next_fiber(me) {
            found = Some(f);
            break;
        }
        end_work(s);
        for _ in 0..32 {
            std::hint::spin_loop();
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    s.spinning.fetch_sub(1, Ordering::SeqCst);
    if found.is_some() && s.queued.load(Ordering::SeqCst) > 0 {
        // (no longer watching, and what else became runnable meanwhile
        // woke nobody: another carrier takes it)
        s.wake_idle();
    }
    found
}

fn carrier_loop(vm: &mut Vm) {
    let s = sched();
    let me = vm.carrier;
    loop {
        if begin_work(s) {
            process_external(vm);
            let f = next_fiber(me);
            if let Some(f) = f {
                run_one(vm, f);
            }
            end_work(s);
            if f.is_some() {
                continue;
            }
        }
        if let Some(f) = spin_for_work(s, me) {
            run_one(vm, f);
            end_work(s);
            continue;
        }
        // Idle. Count ourselves idle *before* the final check, under the
        // lock, so a concurrent `schedule` can't miss us.
        let g = s.idle.lock().unwrap();
        s.idle_count.fetch_add(1, Ordering::SeqCst);
        let found = if begin_work(s) {
            let f = next_fiber(me);
            if f.is_none() {
                end_work(s);
            }
            f
        } else {
            None
        };
        if let Some(f) = found {
            s.idle_count.fetch_sub(1, Ordering::AcqRel);
            drop(g);
            run_one(vm, f);
            end_work(s);
            continue;
        }
        if s.external_len.load(Ordering::Acquire) > 0 && !s.paused.load(Ordering::SeqCst) {
            s.idle_count.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        vm.drop_spare();
        safepoint::enter_safe();
        // Every way of giving carriers work wakes them (schedule, requeue,
        // submit, resume_all), so a regular carrier sleeps until woken: an
        // idle process doesn't wake 10 times a second per carrier. A spare
        // wakes now and then, to see whether it should retire.
        let g = if me < s.n { s.idle_cv.wait(g).unwrap() } else { s.idle_cv.wait_timeout(g, Duration::from_millis(100)).unwrap().0 };
        s.idle_count.fetch_sub(1, Ordering::AcqRel);
        drop(g);
        safepoint::leave_safe();
        if should_retire(s, me) {
            // Hand back anything queued here, then stop.
            let left: Vec<Value> = s.locals[me].lock().unwrap().drain(..).collect();
            s.injector.lock().unwrap().extend(left);
            s.active[me].store(false, Ordering::Release);
            s.wake_idle();
            return;
        }
    }
}

/// A carrier's stack. The reader and `bin/encode` recurse up to 10000 levels
/// before refusing; a debug build spends about 3 KB of stack a level there,
/// so 16 MB overflowed at about 5000. Only the pages touched are committed.
const CARRIER_STACK: usize = 64 << 20;

/// Start the carrier thread for `slot`. Carrier 0 boots (or restores an
/// image) first and reports through `boot`.
fn start_carrier(slot: usize, restore: Option<crate::image::Restore>, boot: Option<mpsc::Sender<Result<(), String>>>) {
    std::thread::Builder::new()
        .name(format!("skein-carrier-{slot}"))
        .stack_size(CARRIER_STACK)
        .spawn(move || {
            CARRIER.with(|c| c.set(slot));
            safepoint::register();
            let mut vm = crate::eval::carrier_vm(slot);
            if let Some(boot_tx) = boot {
                let r = match restore {
                    Some(r) => {
                        crate::image::restore(&mut vm, r);
                        Ok(())
                    }
                    None => crate::eval::boot(&mut vm).map_err(|e| e.to_string()),
                };
                let ok = r.is_ok();
                let _ = boot_tx.send(r);
                if !ok {
                    safepoint::unregister();
                    return;
                }
            }
            carrier_loop(&mut vm);
            drop(vm);
            safepoint::unregister();
        })
        .expect("failed to start carrier thread");
}

// ---- start-up and the public API ----------------------------------------------------------

/// A running Skein runtime: carriers, timer thread, booted skein.core.
pub struct Skein {
    pending: Mutex<HashMap<u64, mpsc::Receiver<RootDone>>>,
}

static SKEIN: OnceLock<Result<Skein, String>> = OnceLock::new();

fn carrier_count() -> usize {
    std::env::var("SKEIN_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4))
}

fn scheduler_roots(f: &mut dyn FnMut(Value)) {
    if let Some(s) = SCHED.get() {
        s.for_each_root(f);
    }
}

/// Block until the runtime has started (by `start` or `start_with` on any
/// thread): the runtime, or None if starting it failed.
pub fn wait_started() -> Option<&'static Skein> {
    SKEIN.wait().as_ref().ok()
}

/// Start the runtime (once per process) and boot skein.core.
pub fn start() -> Result<&'static Skein, Error> {
    start_with(None)
}

/// Start the runtime from an image (`Some(path)`) or by booting skein.core
/// from source (`None`). Once per process: later calls return the runtime
/// started first.
pub fn start_with(image: Option<&std::path::Path>) -> Result<&'static Skein, Error> {
    let skein = SKEIN
        .get_or_init(|| {
            let n = carrier_count();
            skein_rt::heap::init_heap();
            let restore = match image {
                Some(p) => match crate::image::load(p) {
                    Ok(r) => Some(r),
                    Err(e) => return Err(e),
                },
                None => None,
            };
            let _ = SCHED.set(Sched::new(n));
            if let Some(r) = &restore {
                sched().next_id.fetch_max(r.next_fiber_id, Ordering::Relaxed);
            }
            let (boot_tx, boot_rx) = mpsc::channel::<Result<(), String>>();
            let mut restore = Some(restore);
            for id in 0..n {
                let boot = (id == 0).then(|| boot_tx.clone());
                start_carrier(id, if id == 0 { restore.take().flatten() } else { None }, boot);
            }
            std::thread::Builder::new().name("skein-timers".into()).spawn(timer_main).expect("failed to start timer thread");
            std::thread::Builder::new().name("skein-monitor".into()).spawn(monitor_main).expect("failed to start monitor thread");
            crate::reactor::start();
            crate::os::dump_on_sigquit();
            crate::net::raise_fd_limit();
            match boot_rx.recv() {
                Ok(Ok(())) => {
                    // From here on the GC may run: register the roots that live
                    // outside the heap, then let MMTk collect.
                    skein_rt::heap::set_extra_roots(scheduler_roots);
                    skein_rt::heap::enable_collection();
                    Ok(Skein { pending: Mutex::new(HashMap::new()) })
                }
                Ok(Err(e)) => Err(format!("loading core.skn failed: {e}")),
                // (restoring an image can't fail once it has loaded)
                Err(_) => Err("boot carrier died".into()),
            }
        })
        .as_ref()
        .map_err(|e| Error::new(e.clone()))?;
    if image.is_some() {
        // Tell the image it has been restored (on-image-restore hooks).
        static RESTORED: std::sync::Once = std::sync::Once::new();
        let mut r = Ok(());
        RESTORED.call_once(|| r = skein.run(RootTask::Call("%image-restored", String::new()), "user").result.map(|_| ()));
        r?;
    }
    Ok(skein)
}

impl Skein {
    /// Start a root fiber; returns a ticket for `wait` / `cancel`.
    pub fn submit(&self, task: RootTask, ns: &str) -> u64 {
        let s = sched();
        let ticket = s.next_id();
        let (tx, rx) = mpsc::channel();
        let raw = matches!(task, RootTask::Request(_) | RootTask::EvalRaw(_));
        s.notifies.lock().unwrap().insert(ticket, (tx, raw));
        self.pending.lock().unwrap().insert(ticket, rx);
        submit(Job::Root { task, ns: ns.to_owned(), ticket });
        ticket
    }

    /// Wait up to `d` for a root fiber; `None` if it is still running.
    pub fn wait_timeout(&self, ticket: u64, d: Duration) -> Option<RootDone> {
        let rx = self.pending.lock().unwrap().remove(&ticket)?;
        match rx.recv_timeout(d) {
            Ok(done) => Some(done),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.pending.lock().unwrap().insert(ticket, rx);
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Some(RootDone { result: Err(Error::new("root fiber vanished")), ns: "user".into() })
            }
        }
    }

    pub fn wait(&self, ticket: u64) -> RootDone {
        let rx = self.pending.lock().unwrap().remove(&ticket).expect("unknown ticket");
        rx.recv().unwrap_or(RootDone { result: Err(Error::new("root fiber vanished")), ns: "user".into() })
    }

    pub fn run(&self, task: RootTask, ns: &str) -> RootDone {
        let t = self.submit(task, ns);
        self.wait(t)
    }

    /// Evaluate `src` in namespace `ns`; the printed value of the last form.
    pub fn eval(&self, src: &str, ns: &str) -> Result<String, Error> {
        self.run(RootTask::Eval(src.to_owned()), ns).result
    }

    /// Interrupt a root fiber (Ctrl-C).
    pub fn interrupt(&self, ticket: u64) {
        submit(Job::Cancel { ticket, code: fiber::INTERRUPTED });
    }

    pub fn carriers(&self) -> usize {
        sched().n
    }
}

/// Is this waiter still waiting (claim not yet taken)?
pub fn waiter_live(w: Value) -> bool {
    !is_claimed(w.field(waiter::CLAIM))
}

// ---- events (debugger notifications for the REPL and editors) -------------------------

static EVENTS: Mutex<Vec<mpsc::Sender<String>>> = Mutex::new(Vec::new());

/// Receive every event posted from now on (one line of Skein data each).
pub fn subscribe() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    EVENTS.lock().unwrap().push(tx);
    rx
}

/// Broadcast an event to every subscriber (dropping disconnected ones).
pub fn post_event(line: String) {
    EVENTS.lock().unwrap().retain(|tx| tx.send(line.clone()).is_ok());
}
