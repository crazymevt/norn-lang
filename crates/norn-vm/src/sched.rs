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

use crate::vm::{Error, Exit, Vm, cancel_error};
use norn_rt::conc::{fiber, fiber_new, is_claimed, scope, spin_lock, try_claim, waiter};
use norn_rt::object::cons;
use norn_rt::print::pr_str;
use norn_rt::runtime::{Namespace, rt};
use norn_rt::safepoint;
use norn_rt::seq::SeqIter;
use norn_rt::string::str_new;
use norn_rt::vector::vec_from_slice;
use norn_rt::{Kind, Value};
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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

/// Work submitted from outside the carriers (REPL, tests, `norn FILE`).
pub enum RootTask {
    /// Evaluate a string; the result is the printed last value.
    Eval(String),
    /// Evaluate a string, printing each form's value (the REPL).
    Repl(String),
    /// Load a file.
    Load(String),
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
    pub n: usize,
    locals: Vec<Mutex<VecDeque<Value>>>,
    injector: Mutex<VecDeque<Value>>,
    idle: Mutex<()>,
    idle_cv: Condvar,
    idle_count: AtomicUsize,
    timers: Mutex<Timers>,
    timer_cv: Condvar,
    external: Mutex<VecDeque<Job>>,
    external_len: AtomicUsize,
    currents: Vec<AtomicU64>,
    roots: Mutex<HashMap<u64, Value>>,
    notifies: Mutex<HashMap<u64, mpsc::Sender<RootDone>>>,
    next_id: AtomicU64,
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
        Sched {
            n,
            locals: (0..n).map(|_| Mutex::new(VecDeque::new())).collect(),
            injector: Mutex::new(VecDeque::new()),
            idle: Mutex::new(()),
            idle_cv: Condvar::new(),
            idle_count: AtomicUsize::new(0),
            timers: Mutex::new(Timers { heap: BinaryHeap::new(), entries: HashMap::new(), seq: 0 }),
            timer_cv: Condvar::new(),
            external: Mutex::new(VecDeque::new()),
            external_len: AtomicUsize::new(0),
            currents: (0..n).map(|_| AtomicU64::new(Value::NIL.0)).collect(),
            roots: Mutex::new(HashMap::new()),
            notifies: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            spawned: AtomicU64::new(0),
            finished: AtomicU64::new(0),
        }
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn wake_idle(&self) {
        if self.idle_count.load(Ordering::Acquire) > 0 {
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
    if me < s.n {
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
    s.injector.lock().unwrap().push_back(f);
    s.wake_idle();
}

thread_local! {
    static PICKS: Cell<u32> = const { Cell::new(0) };
}

fn next_fiber(me: usize) -> Option<Value> {
    let s = sched();
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
    for k in 1..s.n {
        let victim = (me + k) % s.n;
        if let Some(f) = s.locals[victim].lock().unwrap().pop_back() {
            return Some(f);
        }
    }
    None
}

/// Is anything else waiting to run? (Decides whether a time slice yields.)
pub fn has_other_work(me: usize) -> bool {
    let s = sched();
    (me < s.n && !s.locals[me].lock().unwrap().is_empty()) || !s.injector.lock().unwrap().is_empty()
}

// ---- fibers -------------------------------------------------------------------------

/// Create a fiber running `thunk` (a callable, or an internal `[f arg…]`
/// array) and make it runnable.
pub fn spawn(vm: &mut Vm, thunk: Value, scope_v: Value) -> Value {
    let s = sched();
    let f = fiber_new(&mut vm.m, s.next_id(), thunk, scope_v, vm.ns as *const Namespace as usize);
    if !scope_v.is_nil() {
        let _g = spin_lock(scope_v, scope::LOCK);
        let kids = cons(&mut vm.m, f, scope_v.field(scope::CHILDREN));
        scope_v.set_field(scope::CHILDREN, kids);
        scope_v.set_word(scope::PENDING, scope_v.word(scope::PENDING) + 1);
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
    } else {
        Error::new(pr_str(err, false))
    }
}

/// Rebuild the error a fiber died with.
pub fn error_from_fiber(f: Value) -> Error {
    let mut e = Error::new(pr_str(f.field(fiber::ERROR), false));
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
    let (emsg, etrace) = match &err {
        Some(e) => {
            let msg = str_new(&mut vm.m, &e.msg);
            let lines: Vec<Value> = e.trace.iter().map(|t| str_new(&mut vm.m, t)).collect();
            (msg, vec_from_slice(&mut vm.m, &lines))
        }
        None => (Value::NIL, Value::NIL),
    };
    let joiners = {
        let _g = spin_lock(f, fiber::LOCK);
        f.set_field(fiber::RESULT, val);
        f.set_field(fiber::ERROR, emsg);
        f.set_field(fiber::TRACE, etrace);
        f.set_field(fiber::SEG, Value::NIL);
        aset(f, fiber::PARK, Value::NIL);
        f.fiber_state().store(fiber::DONE, Ordering::Release);
        let j = f.field(fiber::JOINERS);
        f.set_field(fiber::JOINERS, Value::NIL);
        j
    };
    for w in list_items(joiners) {
        if try_claim(w.field(waiter::CLAIM)) {
            if err.is_some() && w.word(waiter::FLAGS) & waiter::NO_RAISE == 0 {
                aset(w.field(waiter::FIBER), fiber::WAKE_ERR, f);
            }
            deliver(w, val, Value::NIL);
        }
    }
    let sc = f.field(fiber::SCOPE);
    if !sc.is_nil() {
        scope_child_done(sc, f, err.is_some());
    }
    let ticket = f.word(fiber::NOTIFY);
    if ticket != 0 {
        s.roots.lock().unwrap().remove(&ticket);
        if let Some(tx) = s.notifies.lock().unwrap().remove(&ticket) {
            let result = match err {
                Some(e) => Err(e),
                None => Ok(pr_str(val, true)),
            };
            let _ = tx.send(RootDone { result, ns: vm.ns.name.sym_name().to_owned() });
        }
    } else if let Some(e) = &err
        && sc.is_nil()
        && e.msg != "Cancelled"
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

fn cancel_children(sc: Value) {
    let kids = {
        let _g = spin_lock(sc, scope::LOCK);
        sc.field(scope::CHILDREN)
    };
    for k in list_items(kids) {
        cancel(k, fiber::CANCELLED);
    }
}

fn scope_child_done(sc: Value, child: Value, failed: bool) {
    let (owner, cancel_siblings) = {
        let _g = spin_lock(sc, scope::LOCK);
        sc.set_word(scope::PENDING, sc.word(scope::PENDING) - 1);
        let mut cancel_siblings = false;
        if failed && sc.field(scope::FAILED).is_nil() {
            sc.set_field(scope::FAILED, child);
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
    let claim = norn_rt::conc::claim_new(&mut vm.m);
    let w = norn_rt::conc::waiter_new(&mut vm.m, me, Value::NIL, claim, Value::NIL, waiter::NO_CANCEL_WAKE);
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
    let claim = norn_rt::conc::claim_new(&mut vm.m);
    let w = norn_rt::conc::waiter_new(&mut vm.m, vm.current, Value::NIL, claim, Value::NIL, 0);
    park_on(vm, w, false);
    add_timer(Instant::now() + Duration::from_millis(ms), TimerKind::Wake, w);
    Ok(Value::NIL)
}

/// `(timeout ms)`: a channel that closes after `ms`.
pub fn timeout(vm: &mut Vm, ms: u64) -> Value {
    let c = norn_rt::conc::chan_new(&mut vm.m, 0);
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
                };
                let sym = rt().symbol(&mut vm.m, None, fname);
                let var = vm.core.lookup(sym).expect("norn.core is missing its root functions");
                let arg = str_new(&mut vm.m, &arg);
                let spec = norn_rt::array::array_from(&mut vm.m, &[var, arg]);
                let ns_sym = rt().symbol(&mut vm.m, None, &ns);
                let ns = rt().find_or_create_ns(ns_sym);
                let f = fiber_new(&mut vm.m, ticket, spec, Value::NIL, ns as *const Namespace as usize);
                f.set_word(fiber::NOTIFY, ticket);
                s.roots.lock().unwrap().insert(ticket, f);
                s.spawned.fetch_add(1, Ordering::Relaxed);
                schedule(f);
            }
            Job::Cancel { ticket, code } => {
                let f = s.roots.lock().unwrap().get(&ticket).copied();
                if let Some(f) = f {
                    cancel(f, code);
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
    // SAFETY: NS holds a leaked &'static Namespace.
    vm.ns = unsafe { &*(f.word(fiber::NS) as *const Namespace) };
    loop {
        aset(f, fiber::PARK, Value::NIL);
        aset(f, fiber::JOINING, Value::NIL);
        let r = if f.fiber_cancel() != 0 {
            Err(cancel_error(f.fiber_cancel() as i64))
        } else {
            vm.run_fiber(f)
        };
        f.set_word(fiber::NS, vm.ns as *const Namespace as usize as u64);
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
                // A cancel that arrived before our waiter was visible in PARK
                // couldn't wake us; we must notice it ourselves, or we would
                // park forever with the cancel flag set.
                if f.fiber_cancel() != 0 {
                    let w = aget(f, fiber::PARK);
                    if !w.is_nil()
                        && w.word(waiter::FLAGS) & waiter::NO_CANCEL_WAKE == 0
                        && try_claim(w.field(waiter::CLAIM))
                    {
                        continue; // the loop top raises Cancelled
                    }
                }
                match f.fiber_state().compare_exchange(
                    fiber::RUNNING,
                    fiber::PARKED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
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

fn carrier_loop(vm: &mut Vm) {
    let s = sched();
    let me = vm.carrier;
    loop {
        process_external(vm);
        if let Some(f) = next_fiber(me) {
            run_one(vm, f);
            continue;
        }
        // Idle. Count ourselves idle *before* the final check, under the
        // lock, so a concurrent `schedule` can't miss us.
        let g = s.idle.lock().unwrap();
        s.idle_count.fetch_add(1, Ordering::AcqRel);
        let found = next_fiber(me);
        if let Some(f) = found {
            s.idle_count.fetch_sub(1, Ordering::AcqRel);
            drop(g);
            run_one(vm, f);
            continue;
        }
        if s.external_len.load(Ordering::Acquire) > 0 {
            s.idle_count.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        vm.drop_spare();
        safepoint::enter_safe();
        let g = s.idle_cv.wait_timeout(g, Duration::from_millis(100)).unwrap().0;
        s.idle_count.fetch_sub(1, Ordering::AcqRel);
        drop(g);
        safepoint::leave_safe();
    }
}

// ---- start-up and the public API ----------------------------------------------------------

/// A running Norn runtime: carriers, timer thread, booted norn.core.
pub struct Norn {
    pending: Mutex<HashMap<u64, mpsc::Receiver<RootDone>>>,
}

static NORN: OnceLock<Result<Norn, String>> = OnceLock::new();

fn carrier_count() -> usize {
    std::env::var("NORN_THREADS")
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

/// Start the runtime (once per process) and boot norn.core.
pub fn start() -> Result<&'static Norn, Error> {
    NORN.get_or_init(|| {
        let n = carrier_count();
        norn_rt::heap::init_heap();
        let _ = SCHED.set(Sched::new(n));
        let (boot_tx, boot_rx) = mpsc::channel::<Result<(), String>>();
        for id in 0..n {
            let boot_tx = boot_tx.clone();
            std::thread::Builder::new()
                .name(format!("norn-carrier-{id}"))
                .stack_size(16 << 20)
                .spawn(move || {
                    CARRIER.with(|c| c.set(id));
                    safepoint::register();
                    let mut vm = crate::eval::carrier_vm(id);
                    if id == 0 {
                        let r = crate::eval::boot(&mut vm).map_err(|e| e.to_string());
                        let ok = r.is_ok();
                        let _ = boot_tx.send(r);
                        if !ok {
                            safepoint::unregister();
                            return;
                        }
                    }
                    carrier_loop(&mut vm);
                })
                .expect("failed to start carrier thread");
        }
        std::thread::Builder::new().name("norn-timers".into()).spawn(timer_main).expect("failed to start timer thread");
        match boot_rx.recv() {
            Ok(Ok(())) => {
                // From here on the GC may run: register the roots that live
                // outside the heap, then let MMTk collect.
                norn_rt::heap::set_extra_roots(scheduler_roots);
                norn_rt::heap::enable_collection();
                Ok(Norn { pending: Mutex::new(HashMap::new()) })
            }
            Ok(Err(e)) => Err(format!("loading core.norn failed: {e}")),
            Err(_) => Err("boot carrier died".into()),
        }
    })
    .as_ref()
    .map_err(|e| Error::new(e.clone()))
}

impl Norn {
    /// Start a root fiber; returns a ticket for `wait` / `cancel`.
    pub fn submit(&self, task: RootTask, ns: &str) -> u64 {
        let s = sched();
        let ticket = s.next_id();
        let (tx, rx) = mpsc::channel();
        s.notifies.lock().unwrap().insert(ticket, tx);
        self.pending.lock().unwrap().insert(ticket, rx);
        submit(Job::Root { task, ns: ns.to_owned(), ticket });
        ticket
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
