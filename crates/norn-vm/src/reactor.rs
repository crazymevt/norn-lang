//! The I/O reactor (runtime.md §5.5): one thread running `mio` (kqueue or
//! epoll) that wakes fibers waiting on sockets and pipes.
//!
//! **Handles.** Each open socket or pipe is an entry in a table keyed by a
//! number, which the Norn `Handle` object carries (with the process epoch,
//! so a handle restored from an image is recognisably dead).
//!
//! **Waiting without lost wake-ups.** Sources are registered edge-triggered.
//! Each entry counts readiness events per direction. An I/O primitive
//! that would block returns `:again`; the Norn wrapper then calls
//! `%io-wait` with the count it read *before* trying. If an event arrived
//! meanwhile the count differs and the fiber retries at once; otherwise it
//! parks on a waiter that the reactor thread wakes, as the timer thread does.
//!
//! Waiters live only here, so they are GC roots (`for_each_root`), and
//! deliveries happen inside `sched::delivering`, which an image save holds
//! off.

use crate::sched;
use crate::vm::{Error, Result, Vm};
use mio::{Events, Interest, Poll, Registry, Token};
use norn_rt::conc::{claim_new, handle, process_epoch, try_claim, waiter, waiter_new};
use norn_rt::runtime::rt;
use norn_rt::{Kind, Mutator, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[allow(dead_code)] // the socket variants come into use with norn.net (M6 part 5)
pub(crate) enum Io {
    PipeIn(mio::unix::pipe::Receiver),
    PipeOut(mio::unix::pipe::Sender),
    TcpListener(mio::net::TcpListener),
    Tcp(mio::net::TcpStream),
    Udp(mio::net::UdpSocket),
    /// Not pollable: read and written on the blocking pool.
    File(std::fs::File),
    /// A file watcher (norn.fs); dropping it stops the watch.
    Watch(Box<dyn std::any::Any + Send>),
    /// A child process (norn.process): waited for on the blocking pool.
    Child(std::process::Child),
}

impl Io {
    /// The mio source, for the kinds the reactor polls.
    fn source(&mut self) -> Option<(&mut dyn mio::event::Source, Interest)> {
        Some(match self {
            Io::PipeIn(r) => (r, Interest::READABLE),
            Io::PipeOut(w) => (w, Interest::WRITABLE),
            Io::TcpListener(l) => (l, Interest::READABLE),
            Io::Tcp(s) => (s, Interest::READABLE | Interest::WRITABLE),
            Io::Udp(s) => (s, Interest::READABLE | Interest::WRITABLE),
            Io::File(_) | Io::Watch(_) | Io::Child(_) => return None,
        })
    }
}

pub(crate) const READ: usize = 0;
pub(crate) const WRITE: usize = 1;

#[derive(Default)]
struct Ready {
    /// Readiness events seen, per direction.
    seq: [u64; 2],
    /// Waiters parked per direction.
    waiters: [Vec<Value>; 2],
    closed: bool,
}

pub(crate) struct Entry {
    /// The source; None once closed.
    pub io: Mutex<Option<Io>>,
    ready: Mutex<Ready>,
}

static TABLE: Mutex<Option<HashMap<u64, Arc<Entry>>>> = Mutex::new(None);
static NEXT: AtomicU64 = AtomicU64::new(1);
static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get().expect("reactor not started")
}

/// Start the reactor thread (once per process).
pub(crate) fn start() {
    let poll = Poll::new().expect("can't create the I/O poller");
    let reg = poll.registry().try_clone().expect("can't clone the poll registry");
    if REGISTRY.set(reg).is_err() {
        return;
    }
    std::thread::Builder::new().name("norn-reactor".into()).spawn(move || run(poll)).expect("failed to start the reactor");
}

fn run(mut poll: Poll) {
    let mut events = Events::with_capacity(512);
    loop {
        if let Err(e) = poll.poll(&mut events, None) {
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            panic!("I/O poll failed: {e}");
        }
        let mut wake = Vec::new();
        for ev in events.iter() {
            let Some(entry) = lookup(ev.token().0 as u64) else { continue };
            let mut r = entry.ready.lock().unwrap();
            if ev.is_readable() || ev.is_read_closed() || ev.is_error() {
                r.seq[READ] += 1;
                wake.append(&mut r.waiters[READ]);
            }
            if ev.is_writable() || ev.is_write_closed() || ev.is_error() {
                r.seq[WRITE] += 1;
                wake.append(&mut r.waiters[WRITE]);
            }
        }
        if !wake.is_empty() {
            sched::delivering(|| wake_all(wake));
        }
    }
}

fn wake_all(ws: Vec<Value>) {
    for w in ws {
        if try_claim(w.field(waiter::CLAIM)) {
            sched::deliver(w, Value::NIL, Value::NIL);
        }
    }
}

fn lookup(id: u64) -> Option<Arc<Entry>> {
    TABLE.lock().unwrap().as_ref().and_then(|t| t.get(&id).cloned())
}

/// Register `io` with the reactor and make its Handle (`kind` names it, as
/// in `#<tcp 3>`).
pub(crate) fn register(m: &mut Mutator, mut io: Io, kind: &str, info: Value) -> std::result::Result<Value, String> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    if let Some((src, interest)) = io.source() {
        registry().register(src, Token(id as usize), interest).map_err(|e| e.to_string())?;
    }
    let entry = Arc::new(Entry { io: Mutex::new(Some(io)), ready: Mutex::new(Ready::default()) });
    TABLE.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, entry);
    let k = rt().keyword(m, None, kind);
    Ok(norn_rt::conc::handle_new(m, id, k, info))
}

/// The entry behind handle `h`, which must be of kind `kind` (e.g. "file").
pub(crate) fn entry_of(h: Value, kind: &str) -> Result<Arc<Entry>> {
    let e = entry(h)?;
    if h.field(handle::KIND).sym_name() != kind {
        return Err(Error::new(format!("Expected a {kind} handle, got a {} handle", h.field(handle::KIND).sym_name())));
    }
    Ok(e)
}

/// The entry behind handle `h`.
pub(crate) fn entry(h: Value) -> Result<Arc<Entry>> {
    if !h.is(Kind::Handle) {
        return Err(Error::new(format!("Expected an I/O handle, got {}", h.type_name())));
    }
    if h.word(handle::EPOCH) != process_epoch() {
        return Err(Error::new("Resource lost: this handle was opened before an image restore"));
    }
    lookup(h.word(handle::ID)).ok_or_else(|| Error::new("The handle is closed"))
}

/// Run `f` on the open source of `h`.
pub(crate) fn with_io<R>(h: Value, f: impl FnOnce(&mut Io) -> R) -> Result<R> {
    let e = entry(h)?;
    let mut io = e.io.lock().unwrap();
    match io.as_mut() {
        Some(io) => Ok(f(io)),
        None => Err(Error::new("The handle is closed")),
    }
}

/// Readiness events seen so far in direction `dir` (read before trying).
pub(crate) fn seq(h: Value, dir: usize) -> Result<u64> {
    Ok(entry(h)?.ready.lock().unwrap().seq[dir])
}

/// Park until direction `dir` of `h` has seen an event after `seen`. Returns
/// without parking if one already has (or the handle is closed): the caller
/// just tries again.
pub(crate) fn wait(vm: &mut Vm, h: Value, dir: usize, seen: u64) -> Result<Value> {
    let Ok(e) = entry(h) else { return Ok(Value::NIL) };
    if !vm.can_park() {
        return Err(Vm::park_error());
    }
    let mut r = e.ready.lock().unwrap();
    if r.closed || r.seq[dir] != seen {
        return Ok(Value::NIL);
    }
    let claim = claim_new(&mut vm.m);
    let w = waiter_new(&mut vm.m, vm.current, Value::NIL, claim, Value::NIL, 0);
    r.waiters[dir].retain(|w| !norn_rt::conc::is_claimed(w.field(waiter::CLAIM)));
    r.waiters[dir].push(w);
    sched::park_on(vm, w, false);
    Ok(Value::NIL)
}

/// Close `h`: the source is dropped (closing the descriptor) and waiters
/// wake to find it closed. Closing twice is harmless.
pub(crate) fn close(h: Value) -> Result<Value> {
    if !h.is(Kind::Handle) || h.word(handle::EPOCH) != process_epoch() {
        return Ok(Value::NIL);
    }
    close_id(h.word(handle::ID));
    Ok(Value::NIL)
}

/// Close by table number (cleanups hold the number, not the handle).
pub(crate) fn close_id(id: u64) {
    let Some(e) = TABLE.lock().unwrap().as_mut().and_then(|t| t.remove(&id)) else { return };
    if let Some(mut io) = e.io.lock().unwrap().take()
        && let Some((src, _)) = io.source()
    {
        let _ = registry().deregister(src);
    }
    let ws: Vec<Value> = {
        let mut r = e.ready.lock().unwrap();
        r.closed = true;
        let [a, b] = std::mem::take(&mut r.waiters);
        a.into_iter().chain(b).collect()
    };
    if !ws.is_empty() {
        sched::delivering(|| wake_all(ws));
    }
}

/// Open handles (for tests and `norn.sys`).
pub(crate) fn open_count() -> usize {
    TABLE.lock().unwrap().as_ref().map_or(0, |t| t.len())
}

/// GC roots: the waiters parked here.
pub(crate) fn for_each_root(f: &mut dyn FnMut(Value)) {
    if let Some(t) = TABLE.lock().unwrap().as_ref() {
        for e in t.values() {
            let r = e.ready.lock().unwrap();
            r.waiters.iter().flatten().for_each(|&w| f(w));
        }
    }
}

/// Is waiter `w` parked on the reactor? (An image saves such fibers dead.)
pub(crate) fn holds_waiter(w: Value) -> bool {
    TABLE.lock().unwrap().as_ref().is_some_and(|t| t.values().any(|e| e.ready.lock().unwrap().waiters.iter().flatten().any(|&x| x == w)))
}
