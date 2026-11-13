//! The blocking pool (runtime.md §5.5): threads that run calls which block
//! in the OS (file I/O, DNS lookups, blocking FFI calls) so carriers never do.
//!
//! A job runs in two halves:
//! - the blocking half, with no access to the heap (the GC may run
//!   meanwhile);
//! - a `Finish` that turns its result into a Norn value. It runs inside
//!   `sched::delivering`, where the pool thread counts as running and
//!   allocates with its own mutator.
//!
//! The caller's fiber parks on a waiter until then. The pool grows on
//! demand up to `MAX_THREADS`, and idle threads above `MIN_THREADS` exit.

use crate::sched;
use crate::vm::{Error, Result, Vm};
use norn_rt::conc::{claim_new, fiber, try_claim, waiter, waiter_new};
use norn_rt::{Mutator, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// Builds the Norn result of a job (on a pool thread, as a running participant).
pub(crate) type Finish = Box<dyn FnOnce(&mut Mutator) -> std::result::Result<Value, String> + Send>;
type Job = Box<dyn FnOnce() -> Finish + Send>;

struct Pending {
    job: Job,
    waiter: Value,
    root: u64,
}

// SAFETY: the Values in Pending are rooted (sched::root) until delivered.
unsafe impl Send for Pending {}

static QUEUE: Mutex<VecDeque<Pending>> = Mutex::new(VecDeque::new());
static CV: Condvar = Condvar::new();
static THREADS: AtomicUsize = AtomicUsize::new(0);
static IDLE: AtomicUsize = AtomicUsize::new(0);
/// Waiters of jobs in progress (an image saves their fibers dead).
static IN_FLIGHT: Mutex<Vec<Value>> = Mutex::new(Vec::new());

const MIN_THREADS: usize = 2;
const MAX_THREADS: usize = 64;

/// Run `job` on the pool and park the current fiber until its result is
/// ready (the primitive's return value). Outside a fiber (boot), run it here.
pub(crate) fn run(vm: &mut Vm, job: impl FnOnce() -> Finish + Send + 'static) -> Result<Value> {
    if !vm.can_park() {
        let finish = job();
        return finish(&mut vm.m).map_err(Error::new);
    }
    let me = vm.current;
    let claim = claim_new(&mut vm.m);
    // The job can't be interrupted: a cancel takes effect when it finishes.
    let w = waiter_new(&mut vm.m, me, Value::NIL, claim, Value::NIL, waiter::NO_CANCEL_WAKE);
    sched::park_on(vm, w, false);
    let root = sched::root(me);
    IN_FLIGHT.lock().unwrap().push(w);
    let mut q = QUEUE.lock().unwrap();
    q.push_back(Pending { job: Box::new(job), waiter: w, root });
    if IDLE.load(Ordering::Acquire) == 0 && THREADS.load(Ordering::Acquire) < MAX_THREADS {
        THREADS.fetch_add(1, Ordering::AcqRel);
        std::thread::Builder::new().name("norn-blocking".into()).spawn(worker).map_err(|e| Error::new(e.to_string()))?;
    }
    CV.notify_one();
    Ok(Value::NIL)
}

fn worker() {
    let mut m = norn_rt::safepoint::while_running(Mutator::new);
    loop {
        let job = {
            let mut q = QUEUE.lock().unwrap();
            loop {
                if let Some(p) = q.pop_front() {
                    break Some(p);
                }
                IDLE.fetch_add(1, Ordering::AcqRel);
                let (g, t) = CV.wait_timeout(q, Duration::from_secs(30)).unwrap();
                q = g;
                IDLE.fetch_sub(1, Ordering::AcqRel);
                if t.timed_out() && q.is_empty() && THREADS.load(Ordering::Acquire) > MIN_THREADS {
                    break None;
                }
            }
        };
        let Some(p) = job else { break };
        let finish = (p.job)();
        let w = p.waiter;
        sched::delivering(|| {
            let r = finish(&mut m);
            if try_claim(w.field(waiter::CLAIM)) {
                match r {
                    Ok(v) => sched::deliver(w, v, Value::NIL),
                    Err(e) => {
                        let msg = norn_rt::string::str_new(&mut m, &e);
                        crate::sched::aset(w.field(waiter::FIBER), fiber::WAKE_ERR, msg);
                        sched::deliver(w, Value::NIL, Value::NIL);
                    }
                }
            }
        });
        IN_FLIGHT.lock().unwrap().retain(|&x| x != w);
        sched::unroot(p.root);
    }
    THREADS.fetch_sub(1, Ordering::AcqRel);
    norn_rt::safepoint::while_running(move || drop(m));
}

/// Is waiter `w` waiting for the pool?
pub(crate) fn holds_waiter(w: Value) -> bool {
    IN_FLIGHT.lock().unwrap().contains(&w)
}

/// Pool threads now running (for `norn.sys` and tests).
pub(crate) fn threads() -> usize {
    THREADS.load(Ordering::Acquire)
}
