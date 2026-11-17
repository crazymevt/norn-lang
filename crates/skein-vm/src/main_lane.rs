//! The main-thread lane: foreign calls that must run on one particular OS
//! thread, the process's first one. macOS requires window, event and many
//! graphics calls there, and an OpenGL context stays with the thread that
//! made it current. The `skein` command gives its first thread to `serve`
//! and runs everything else on others.
//!
//! A call works as on the blocking pool (`blocking.rs`): the caller's fiber
//! parks while the lane thread runs the job, which doesn't touch the heap,
//! and a `Finish` then builds the Skein result. Jobs run one at a time, in
//! the order they were submitted.
//!
//! A C callback the lane thread makes during a job goes to the fiber
//! serving that callback, as from any thread of C's own. While it waits for
//! the answer, the lane thread keeps running jobs, so the callback can make
//! main-thread calls of its own (raylib calling Skein calling raylib).

use crate::blocking::Finish;
use crate::sched;
use crate::vm::{Error, Failure, Result, Vm};
use skein_rt::conc::{claim_new, fiber, try_claim, waiter, waiter_new};
use skein_rt::{Mutator, Value};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::ThreadId;

type Job = Box<dyn FnOnce() -> Finish + Send>;
type SyncReply = Arc<(Mutex<Option<Finish>>, Condvar)>;

enum Reply {
    /// A parked fiber, woken through its waiter (rooted until delivered).
    Fiber { waiter: Value, root: u64 },
    /// A caller that can't park (boot, macro expansion) waits on a condvar
    /// and builds the result itself.
    Sync(SyncReply),
}

struct Pending {
    job: Job,
    reply: Reply,
}

// SAFETY: the Values in Pending are rooted (sched::root) until delivered.
unsafe impl Send for Pending {}

static QUEUE: Mutex<VecDeque<Pending>> = Mutex::new(VecDeque::new());
static CV: Condvar = Condvar::new();
static SERVED: AtomicBool = AtomicBool::new(false);
static LANE_THREAD: OnceLock<ThreadId> = OnceLock::new();
/// Waiters of jobs queued or in progress (an image saves their fibers dead).
static IN_FLIGHT: Mutex<Vec<Value>> = Mutex::new(Vec::new());

thread_local! {
    /// The lane thread's mutator, for delivering results.
    static LANE_MUTATOR: RefCell<Option<Mutator>> = const { RefCell::new(None) };
}

/// Is this thread the one serving the lane?
pub(crate) fn on_lane_thread() -> bool {
    LANE_THREAD.get().is_some_and(|&t| t == std::thread::current().id())
}

/// Does a thread serve the lane?
pub fn served() -> bool {
    SERVED.load(Ordering::Acquire)
}

/// Run `job` on the lane thread and park the current fiber until its result
/// is ready (the primitive's return value). A caller that can't park waits.
pub(crate) fn run(vm: &mut Vm, job: impl FnOnce() -> Finish + Send + 'static) -> Result<Value> {
    if on_lane_thread() {
        let finish = job();
        return finish(&mut vm.m).map_err(Failure::into_error);
    }
    if !served() {
        return Err(Error::new(
            "No thread serves main-thread calls: run the program with the skein command, \
             or have the program embedding Skein call skein_vm::main_lane::serve on its first thread",
        ));
    }
    if !vm.can_park() {
        let reply: SyncReply = Arc::new((Mutex::new(None), Condvar::new()));
        submit(Pending { job: Box::new(job), reply: Reply::Sync(reply.clone()) });
        // (other carriers may collect while this one waits)
        let finish = vm.native_region(&[], || {
            let (lock, cv) = &*reply;
            let mut got = lock.lock().unwrap();
            loop {
                if let Some(f) = got.take() {
                    break f;
                }
                got = cv.wait(got).unwrap();
            }
        });
        return finish(&mut vm.m).map_err(Failure::into_error);
    }
    let me = vm.current;
    let claim = claim_new(&mut vm.m);
    // The job can't be interrupted: a cancel takes effect when it finishes.
    let w = waiter_new(&mut vm.m, me, Value::NIL, claim, Value::NIL, waiter::NO_CANCEL_WAKE);
    sched::park_on(vm, w, false);
    let root = sched::root(me);
    IN_FLIGHT.lock().unwrap().push(w);
    submit(Pending { job: Box::new(job), reply: Reply::Fiber { waiter: w, root } });
    Ok(Value::NIL)
}

fn submit(p: Pending) {
    QUEUE.lock().unwrap().push_back(p);
    CV.notify_all();
}

/// Wake the lane thread to look at its queue and its wait condition again
/// (a callback it waits for has been answered).
pub(crate) fn wake() {
    let _q = QUEUE.lock().unwrap();
    CV.notify_all();
}

/// Serve the lane on this thread, for as long as the process lives. Call it
/// on the process's first thread once the runtime has started; the `skein`
/// command does. Panics if another thread already serves it.
pub fn serve() -> ! {
    let me = std::thread::current().id();
    if *LANE_THREAD.get_or_init(|| me) != me {
        panic!("skein: another thread already serves main-thread calls");
    }
    LANE_MUTATOR.with(|m| *m.borrow_mut() = Some(skein_rt::safepoint::while_running(Mutator::new)));
    SERVED.store(true, Ordering::Release);
    loop {
        run_until(&|| false);
    }
}

/// Run queued jobs on the lane thread until `done` says to stop. `done` is
/// checked with the queue locked, so `wake` after making it true is never
/// missed.
pub(crate) fn run_until(done: &dyn Fn() -> bool) {
    loop {
        let next = {
            let mut q = QUEUE.lock().unwrap();
            loop {
                if done() {
                    return;
                }
                if let Some(p) = q.pop_front() {
                    break p;
                }
                q = CV.wait(q).unwrap();
            }
        };
        process(next);
    }
}

fn process(p: Pending) {
    let finish = (p.job)();
    match p.reply {
        Reply::Sync(reply) => {
            let (lock, cv) = &*reply;
            *lock.lock().unwrap() = Some(finish);
            cv.notify_all();
        }
        Reply::Fiber { waiter: w, root } => {
            LANE_MUTATOR.with(|cell| {
                let mut cell = cell.borrow_mut();
                let m = cell.as_mut().expect("the lane thread has a mutator");
                sched::delivering(|| {
                    let r = finish(m);
                    if try_claim(w.field(waiter::CLAIM)) {
                        match r {
                            Ok(v) => sched::deliver(w, v, Value::NIL),
                            Err(e) => {
                                let err = crate::vm::failure_value(m, &e);
                                sched::aset(w.field(waiter::FIBER), fiber::WAKE_ERR, err);
                                sched::deliver(w, Value::NIL, Value::NIL);
                            }
                        }
                    }
                });
            });
            IN_FLIGHT.lock().unwrap().retain(|&x| x != w);
            sched::unroot(root);
        }
    }
}

/// Is waiter `w` waiting for the lane?
pub(crate) fn holds_waiter(w: Value) -> bool {
    IN_FLIGHT.lock().unwrap().contains(&w)
}
